use super::*;

fn state() -> SettingsState {
    SettingsState::new(
        "C:\\cfg\\kalkan.toml".into(),
        FileState::Loaded,
        Config::default(),
    )
}

#[test]
fn form_round_trips_the_config() {
    let mut c = Config::default();
    c.logging.level = LogLevel::Debug;
    c.logging.max_file_size_mb = 12;
    c.monitor.sample_interval_ms = 500;
    c.ui.start_minimized = true;
    let f = SettingsForm::from_config(&c);
    assert_eq!(f.to_config(&Config::default()).unwrap(), c);
}

#[test]
fn non_numbers_and_empty_values_are_explained_not_coerced() {
    let mut f = SettingsForm::from_config(&Config::default());
    f.max_file_size_mb = "ten".into();
    f.sample_interval_ms = "  ".into();
    let errs = f.to_config(&Config::default()).unwrap_err();
    assert_eq!(errs.len(), 2);
    assert_eq!(errs[0].field, Some(Field::MaxFileSizeMb));
    assert_eq!(errs[0].message, "\"ten\" is not a whole number");
    assert_eq!(errs[1].field, Some(Field::SampleIntervalMs));
    assert_eq!(errs[1].message, "a value is required");
    assert_eq!(
        errs[0].to_string(),
        "Maximum log file size: \"ten\" is not a whole number"
    );
}

#[test]
fn out_of_range_values_use_the_loaders_rules() {
    let mut f = SettingsForm::from_config(&Config::default());
    f.max_file_size_mb = "0".into();
    f.max_log_files = "91".into();
    f.sample_interval_ms = "99999999999".into(); // saturates, still rejected
    let errs = f.to_config(&Config::default()).unwrap_err();
    let fields: Vec<_> = errs.iter().map(|e| e.field).collect();
    assert_eq!(
        fields,
        vec![
            Some(Field::SampleIntervalMs),
            Some(Field::MaxLogFiles),
            Some(Field::MaxFileSizeMb)
        ]
    );
    assert!(
        errs[1].message.contains("allowed 1..=90"),
        "{}",
        errs[1].message
    );
}

#[test]
fn whitespace_around_numbers_is_accepted() {
    let mut f = SettingsForm::from_config(&Config::default());
    f.max_log_files = " 12 ".into();
    assert_eq!(
        f.to_config(&Config::default())
            .unwrap()
            .logging
            .max_log_files,
        12
    );
}

#[test]
fn unedited_parts_of_the_base_config_are_preserved() {
    let base = Config {
        schema_version: 1,
        ..Config::default()
    };
    let f = SettingsForm::from_config(&base);
    assert_eq!(f.to_config(&base).unwrap().schema_version, 1);
}

#[test]
fn dirty_revert_and_defaults() {
    let mut s = state();
    assert!(!s.dirty());
    s.form.set_toggle(Field::StartMinimized, true);
    s.form.set_text(Field::MaxLogFiles, "3".into());
    assert!(s.dirty());
    s.revert();
    assert!(!s.dirty());

    let mut custom = Config::default();
    custom.logging.level = LogLevel::Error;
    let mut s = SettingsState::new("p".into(), FileState::Loaded, custom);
    s.load_defaults_into_form();
    assert!(
        s.dirty(),
        "defaults differ from the saved values until saved"
    );
    assert_eq!(s.form, SettingsForm::from_config(&Config::default()));
    let c = s.form.to_config(&s.saved).unwrap();
    s.saved_as(c);
    assert!(!s.dirty());
    assert_eq!(s.saved, Config::default());
}

#[test]
fn saving_an_invalid_file_marks_it_loaded() {
    let mut s = SettingsState::new(
        "p".into(),
        FileState::Invalid("bad".into()),
        Config::default(),
    );
    s.saving = true;
    s.saved_as(Config::default());
    assert_eq!(s.file, FileState::Loaded);
    assert!(!s.saving);
}

#[test]
fn errors_are_attached_to_fields() {
    let mut s = state();
    s.form.max_log_files = "x".into();
    s.errors = s.form.to_config(&s.saved).unwrap_err();
    assert!(s.error_for(Field::MaxLogFiles).is_some());
    assert!(s.error_for(Field::MaxFileSizeMb).is_none());
}

#[test]
fn changed_fields_and_save_message() {
    let old = Config::default();
    let mut new = old.clone();
    new.logging.level = LogLevel::Debug;
    new.monitor.sample_interval_ms = 2000;
    new.logging.file_logging = false;
    let changed = changed_fields(&old, &new);
    assert_eq!(
        changed,
        vec![Field::LogLevel, Field::FileLogging, Field::SampleIntervalMs]
    );
    let m = save_message(&changed);
    assert!(
        m.contains("Sample interval applies the next time the engine starts"),
        "{m}"
    );
    assert!(
        m.contains("Write logs to a file applies when KALKAN is restarted"),
        "{m}"
    );
    assert_eq!(save_message(&[]), "Settings saved (no values changed).");
    assert_eq!(save_message(&[Field::LogLevel]), "Settings saved.");
}

#[test]
fn every_field_maps_to_a_real_config_key_with_help() {
    for f in Field::ALL {
        assert_eq!(Field::from_key(f.key()), Some(f));
        assert!(!f.help().is_empty() && !f.label().is_empty());
    }
}
