use super::*;
use std::thread;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kalkan-rot-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn line(i: usize) -> String {
    // Fixed width so file sizes are predictable.
    format!("2026-01-01T00:00:00.000000Z  INFO t kalkan: line {i:06}\n")
}

const LINE: u64 = 56;

#[test]
fn line_width_is_fixed() {
    assert_eq!(line(1).len() as u64, LINE);
}

#[test]
fn rotates_before_exceeding_the_limit_and_keeps_n_archives() {
    let d = dir("limit");
    let active = d.join("kalkan.log");
    let policy = RotationPolicy {
        max_bytes: LINE * 10,
        keep: 3,
    };
    let mut f = RotatingFile::open(&active, policy).unwrap();
    for i in 0..100 {
        f.write_all(line(i).as_bytes()).unwrap();
        assert!(
            f.size() <= policy.max_bytes,
            "active file over limit at line {i}"
        );
    }
    f.flush().unwrap();
    assert_eq!(
        existing_archives(&active),
        vec![1, 2, 3],
        "exactly `keep` archives"
    );
    for p in [
        active.clone(),
        archive_path(&active, 1),
        archive_path(&active, 3),
    ] {
        let len = fs::metadata(&p).unwrap().len();
        assert!(len <= policy.max_bytes && len % LINE == 0, "{p:?}: {len}");
    }
    // Newest data in the active file, then .1, .2, .3 going back in time.
    let last = fs::read_to_string(&active).unwrap();
    assert!(last.ends_with(&line(99)));
    let a1 = fs::read_to_string(archive_path(&active, 1)).unwrap();
    assert!(a1.ends_with(&line(89)), "{a1}");
    assert_eq!(f.status().rotations, 9);
    assert_eq!(f.status().archives, 3);
    assert!(f.status().last_error.is_none());
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn restart_continues_the_existing_file_and_rotates_if_already_full() {
    let d = dir("restart");
    let active = d.join("kalkan.log");
    let policy = RotationPolicy {
        max_bytes: LINE * 5,
        keep: 2,
    };
    {
        let mut f = RotatingFile::open(&active, policy).unwrap();
        for i in 0..3 {
            f.write_all(line(i).as_bytes()).unwrap();
        }
    }
    let f = RotatingFile::open(&active, policy).unwrap();
    assert_eq!(f.size(), 3 * LINE, "size is re-read after a restart");
    drop(f);
    // A file that is already at the limit is rotated at open.
    fs::write(&active, line(0).repeat(5)).unwrap();
    let f = RotatingFile::open(&active, policy).unwrap();
    assert_eq!(f.size(), 0);
    assert_eq!(existing_archives(&active), vec![1]);
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn reducing_retention_prunes_old_archives_but_never_the_active_log() {
    let d = dir("prune");
    let active = d.join("kalkan.log");
    let mut f = RotatingFile::open(
        &active,
        RotationPolicy {
            max_bytes: LINE,
            keep: 5,
        },
    )
    .unwrap();
    for i in 0..10 {
        f.write_all(line(i).as_bytes()).unwrap();
    }
    assert_eq!(existing_archives(&active), vec![1, 2, 3, 4, 5]);
    f.set_policy(RotationPolicy {
        max_bytes: LINE,
        keep: 2,
    });
    assert_eq!(existing_archives(&active), vec![1, 2]);
    assert!(active.exists());
    // keep = 1 with many rotations: the active file always exists.
    f.set_policy(RotationPolicy {
        max_bytes: LINE,
        keep: 1,
    });
    for i in 0..10 {
        f.write_all(line(i).as_bytes()).unwrap();
        assert!(active.exists());
    }
    assert_eq!(existing_archives(&active), vec![1]);
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn unrelated_files_are_never_deleted() {
    let d = dir("unrelated");
    let active = d.join("kalkan.log");
    for name in [
        "kalkan.log.old",
        "kalkan.backup.log",
        "kalkan.9x.log",
        "other.7.log",
        "kalkan.0.log",
    ] {
        fs::write(d.join(name), "keep me").unwrap();
    }
    let mut f = RotatingFile::open(
        &active,
        RotationPolicy {
            max_bytes: LINE,
            keep: 1,
        },
    )
    .unwrap();
    for i in 0..5 {
        f.write_all(line(i).as_bytes()).unwrap();
    }
    for name in [
        "kalkan.log.old",
        "kalkan.backup.log",
        "kalkan.9x.log",
        "other.7.log",
        "kalkan.0.log",
    ] {
        assert!(d.join(name).exists(), "{name} must survive");
    }
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn rotation_failure_is_reported_and_logging_continues() {
    let d = dir("fail");
    let active = d.join("kalkan.log");
    // A directory where archive 1 must go makes the rename fail.
    fs::create_dir_all(archive_path(&active, 1).join("blocker")).unwrap();
    let mut f = RotatingFile::open(
        &active,
        RotationPolicy {
            max_bytes: LINE * 2,
            keep: 1,
        },
    )
    .unwrap();
    for i in 0..6 {
        f.write_all(line(i).as_bytes())
            .expect("writes keep working");
    }
    let st = f.status();
    let err = st.last_error.expect("rotation error is recorded");
    assert!(err.starts_with("log rotation failed"), "{err}");
    assert_eq!(
        fs::read_to_string(&active).unwrap().lines().count(),
        6,
        "nothing lost"
    );
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn concurrent_writers_never_split_lines_across_rotation() {
    let d = dir("concurrent");
    let active = d.join("kalkan.log");
    let policy = RotationPolicy {
        max_bytes: LINE * 40,
        keep: 4,
    };
    let shared = SharedFile::new(RotatingFile::open(&active, policy).unwrap());
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let s = shared.clone();
            thread::spawn(move || {
                use tracing_subscriber::fmt::MakeWriter;
                for i in 0..500 {
                    let mut w = s.make_writer();
                    w.write_all(line(t * 10_000 + i).as_bytes()).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let st = shared.lock().status();
    assert!(
        st.rotations >= 90,
        "4000 lines / 40 per file: {}",
        st.rotations
    );
    assert!(st.last_error.is_none(), "{:?}", st.last_error);
    let mut files = vec![active.clone()];
    files.extend(
        existing_archives(&active)
            .into_iter()
            .map(|n| archive_path(&active, n)),
    );
    assert_eq!(files.len(), 5, "active + 4 archives");
    for p in files {
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.len() as u64 <= policy.max_bytes, "{p:?} over limit");
        for l in text.lines() {
            assert_eq!(l.len() as u64 + 1, LINE, "torn line in {p:?}: {l:?}");
            assert!(l.starts_with("2026-01-01T"), "{l:?}");
        }
    }
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn closing_during_concurrent_logging_is_safe_and_counted() {
    let d = dir("shutdown");
    let active = d.join("kalkan.log");
    let shared = SharedFile::new(
        RotatingFile::open(
            &active,
            RotationPolicy {
                max_bytes: LINE * 50,
                keep: 2,
            },
        )
        .unwrap(),
    );
    let writers: Vec<_> = (0..4)
        .map(|t| {
            let s = shared.clone();
            thread::spawn(move || {
                use tracing_subscriber::fmt::MakeWriter;
                for i in 0..2000 {
                    let mut w = s.make_writer();
                    w.write_all(line(t * 10_000 + i).as_bytes()).unwrap();
                }
            })
        })
        .collect();
    thread::sleep(std::time::Duration::from_millis(2));
    shared.lock().close(); // "application shutdown" mid-stream
    for w in writers {
        w.join().expect("no writer panics");
    }
    let st = shared.lock().status();
    let written: usize = [
        active.clone(),
        archive_path(&active, 1),
        archive_path(&active, 2),
    ]
    .iter()
    .filter_map(|p| fs::read_to_string(p).ok())
    .map(|t| {
        for l in t.lines() {
            assert_eq!(l.len() as u64 + 1, LINE, "torn line after close: {l:?}");
        }
        t.lines().count()
    })
    .sum();
    assert!(
        st.dropped_writes > 0 || written == 8000,
        "writes after close are counted"
    );
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn archive_naming() {
    assert_eq!(
        archive_path(Path::new("/x/kalkan.log"), 3),
        PathBuf::from("/x/kalkan.3.log")
    );
    assert_eq!(
        archive_path(Path::new("/x/kalkan"), 2),
        PathBuf::from("/x/kalkan.2")
    );
    assert_eq!(
        RotationPolicy::from_mb(5, 7),
        RotationPolicy {
            max_bytes: 5 * 1024 * 1024,
            keep: 7
        }
    );
    assert_eq!(
        RotationPolicy::from_mb(0, 0),
        RotationPolicy {
            max_bytes: 1024 * 1024,
            keep: 1
        }
    );
}
