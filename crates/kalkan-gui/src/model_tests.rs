use super::*;
use kalkan_core::InterfaceKind;
use std::time::Duration;

fn info() -> AppInfo {
    AppInfo {
        log_level: "info".into(),
        elevated: Some(false),
        sample_interval_ms: 1000,
        ..AppInfo::default()
    }
}

fn iface(name: &str, counted: bool, connected: bool) -> InterfaceStatus {
    InterfaceStatus {
        id: name.len() as u64,
        name: name.into(),
        description: format!("{name} adapter"),
        kind: InterfaceKind::Ethernet,
        connected,
        counted,
        rx_bytes: 5 * 1024 * 1024,
        tx_bytes: 1024 * 1024,
        rx_rate: Some(2048),
        tx_rate: Some(1024),
        link_speed_bps: Some(1_000_000_000),
    }
}

fn running_with_network() -> UiState {
    let mut ui = UiState::default();
    ui.apply(Event::StateChanged {
        from: EngineState::Starting,
        to: EngineState::Running,
    });
    ui.apply(Event::Status(EngineStatus {
        state: Some(EngineState::Running),
        cpu_percent: Some(1.3),
        memory_bytes: Some(5 * 1024 * 1024),
        network: Some(NetworkStatus {
            interfaces: vec![iface("Ethernet", true, true), iface("VPN", false, true)],
            rx_rate: Some(2048),
            tx_rate: Some(1024),
            session_rx_bytes: 10 * 1024,
            session_tx_bytes: 4 * 1024,
            peak_rx_rate: 4096,
            peak_tx_rate: 2048,
            session_duration: Duration::from_secs(10),
            samples: 11,
        }),
        network_error: None,
        running_for: Some(Duration::from_secs(65)),
    }));
    ui
}

fn all_values(v: &PageView) -> Vec<&Value> {
    v.cards
        .iter()
        .map(|c| &c.value)
        .chain(v.strip.iter().map(|s| &s.value))
        .chain(v.tiles.iter().map(|t| &t.value))
        .collect()
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
fn no_engine_data_means_no_live_values_anywhere() {
    let ui = UiState::default();
    for p in Page::ALL {
        let v = page_view(p, &ui, &info());
        for val in all_values(&v) {
            if let Value::Real { source, .. } = val {
                assert_ne!(*source, Source::Engine, "{p:?} claims live data: {val:?}");
            }
        }
        if let Some(t) = &v.table {
            if p == Page::Network {
                assert!(t.rows.is_empty(), "no interface rows without the engine");
                assert_eq!(t.empty.as_deref(), Some(NET_NEEDS_RUNNING));
            }
        }
    }
}

#[test]
fn live_network_values_come_only_from_engine_status() {
    let ui = running_with_network();
    let d = page_view(Page::Dashboard, &ui, &info());
    assert_eq!(d.cards[0].value, Value::real("2.0 KB/s", Source::Engine));
    assert_eq!(d.cards[1].value, Value::real("1.0 KB/s", Source::Engine));
    assert_eq!(d.cards[0].detail, "10.0 KB received this session");
    assert_eq!(
        d.strip[0].value,
        Value::real("Ethernet · 1 Gbps", Source::Engine)
    );
    assert_eq!(
        d.strip[1].value,
        Value::real("2 connected of 2", Source::Engine)
    );

    let n = page_view(Page::Network, &ui, &info());
    assert_eq!(n.tiles.len(), 2);
    assert_eq!(n.tiles[0].detail, "Peak 4.0 KB/s");
    let t = n.table.unwrap();
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.rows[0].cells[0], "Ethernet");
    assert_eq!(t.rows[0].status, RowStatus::Connected);
    assert_eq!(t.rows[1].status, RowStatus::Virtual);
    assert!(t.rows[1].muted, "not-counted interface is muted");
    assert!(t.rows.iter().all(|r| r.cells.len() == t.columns.len()));

    let s = page_view(Page::Statistics, &ui, &info());
    assert_eq!(s.cards[0].value, Value::real("10.0 KB", Source::Engine));
    assert_eq!(
        s.cards[4].value,
        Value::real("1.0 KB/s", Source::Engine),
        "10 KB / 10 s"
    );
    assert_eq!(s.strip[0].value, Value::real("01:05", Source::Engine));
    assert_eq!(s.strip[1].value, Value::real("11", Source::Engine));
    assert_eq!(s.strip[2].value, Value::real("1000 ms", Source::Config));
}

#[test]
fn network_error_is_explained_instead_of_numbers() {
    let mut ui = running_with_network();
    ui.status.network = None;
    ui.status.network_error = Some("GetIfTable2 failed with error 87".into());
    let d = page_view(Page::Dashboard, &ui, &info());
    assert!(!d.cards[0].value.is_real());
    assert_eq!(
        d.cards[0].detail,
        "Network monitor unavailable: GetIfTable2 failed with error 87"
    );
    let n = page_view(Page::Network, &ui, &info());
    assert!(n.table.unwrap().empty.unwrap().contains("error 87"));
    assert!(
        page_view(Page::Dashboard, &ui, &info()).cards[2]
            .value
            .is_real(),
        "CPU unaffected"
    );
}

#[test]
fn first_network_sample_shows_waiting_not_zero() {
    let mut ui = running_with_network();
    if let Some(n) = ui.status.network.as_mut() {
        n.rx_rate = None;
        n.tx_rate = None;
    }
    let d = page_view(Page::Dashboard, &ui, &info());
    assert_eq!(d.cards[0].detail, NET_FIRST_SAMPLES);
}

#[test]
fn everything_live_is_cleared_when_the_engine_leaves_running() {
    for to in [EngineState::Stopping, EngineState::Failed] {
        let mut ui = running_with_network();
        ui.apply(Event::StateChanged {
            from: EngineState::Running,
            to,
        });
        for p in Page::ALL {
            for val in all_values(&page_view(p, &ui, &info())) {
                assert!(
                    !matches!(
                        val,
                        Value::Real {
                            source: Source::Engine,
                            ..
                        }
                    ),
                    "{to} {p:?} {val:?}"
                );
            }
        }
    }
}

#[test]
fn history_ring_buffer() {
    let mut h = History::default();
    assert!(h.is_empty() && h.max(Series::Download) == 0);
    for i in 0..(HISTORY_LEN as u64 + 5) {
        h.push(i, i * 2);
    }
    assert_eq!(h.len(), HISTORY_LEN);
    let rx: Vec<_> = h.iter(Series::Download).collect();
    assert_eq!(rx.first(), Some(&5), "oldest entries dropped");
    assert_eq!(rx.last(), Some(&(HISTORY_LEN as u64 + 4)));
    assert_eq!(h.max(Series::Upload), (HISTORY_LEN as u64 + 4) * 2);
    h.clear();
    assert_eq!(h.iter(Series::Upload).count(), 0);
}

#[test]
fn history_follows_engine_samples_and_resets_on_start() {
    let mut ui = running_with_network();
    assert_eq!(ui.history.len(), 1);
    ui.apply(Event::EngineStarted);
    assert!(ui.history.is_empty());
}

#[test]
fn events_update_state_notices_and_counts() {
    let mut ui = UiState::default();
    assert!(ui.apply(Event::DiagnosticMessage {
        level: DiagLevel::Warning,
        message: "Start ignored".into()
    }));
    assert_eq!(ui.notice.as_ref().unwrap().kind, NoticeKind::Warning);
    assert!(ui.apply(Event::DiagnosticMessage {
        level: DiagLevel::Info,
        message: "Wi-Fi disconnected".into()
    }));
    assert_eq!(ui.notice.as_ref().unwrap().kind, NoticeKind::Info);
    assert!(ui.apply(Event::EngineError {
        message: "bad".into()
    }));
    assert_eq!(ui.notice.as_ref().unwrap().kind, NoticeKind::Error);
    assert!(ui.apply(Event::StateChanged {
        from: EngineState::Stopped,
        to: EngineState::Starting
    }));
    assert!(ui.can_stop() && !ui.can_start(), "Stop cancels a start");
    assert!(ui.apply(Event::EngineStarted));
    assert!(ui.notice.is_none(), "a successful start clears old notices");
    ui.apply(Event::EngineStopped);
    assert_eq!(
        ui.counts,
        EventCounts {
            started: 1,
            stopped: 1,
            errors: 1,
            diagnostics: 2
        }
    );
    assert!(!ui.apply(Event::ShutdownComplete));
    assert!(ui.shutdown_complete);
}

#[test]
fn button_affordances_follow_engine_state() {
    let mut ui = UiState::default();
    for (to, start, stop) in [
        (EngineState::Starting, false, true),
        (EngineState::Running, false, true),
        (EngineState::Stopping, false, false),
        (EngineState::Stopped, true, false),
        (EngineState::Failed, true, false),
    ] {
        ui.apply(Event::StateChanged {
            from: ui.engine,
            to,
        });
        assert_eq!((ui.can_start(), ui.can_stop()), (start, stop), "{to}");
    }
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
fn settings_and_logs_pages() {
    let mut i = info();
    i.settings = vec![("Log level".into(), "info".into())];
    i.config_path = "C:\\x\\kalkan.toml".into();
    let v = page_view(Page::Settings, &UiState::default(), &i);
    let t = v.table.unwrap();
    assert_eq!(
        t.rows[0].cells,
        vec!["Log level".to_owned(), "info".to_owned()]
    );
    assert_eq!(v.cards[0].detail, "C:\\x\\kalkan.toml");

    i.log_file = None;
    i.log_note = "Disabled by logging.file_logging".into();
    let v = page_view(Page::Logs, &UiState::default(), &i);
    let card = v.cards.iter().find(|c| c.title == "Log file").unwrap();
    assert!(!card.value.is_real());
    assert_eq!(card.detail, "Disabled by logging.file_logging");
}

#[test]
fn formatting() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(1023), "1023 B");
    assert_eq!(format_bytes(1024), "1.0 KB");
    assert_eq!(format_bytes(1536 * 1024), "1.5 MB");
    assert_eq!(format_rate(2048), "2.0 KB/s");
    assert!(format_bytes(u64::MAX).ends_with("TB"));
    assert_eq!(format_duration(Duration::from_secs(5)), "00:05");
    assert_eq!(format_duration(Duration::from_secs(3723)), "1:02:03");
    assert_eq!(format_link_speed(1_000_000_000), "1 Gbps");
    assert_eq!(format_link_speed(866_700_000), "866.7 Mbps");
    assert_eq!(format_link_speed(100_000_000), "100 Mbps");
    assert_eq!(format_link_speed(56_000), "56 kbps");
}
