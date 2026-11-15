//! KALKAN entry point.
//!
//! * No arguments (or only `--config`): open the native GUI.
//! * CLI options (`--version`, `--check-config`, ...): print and exit. The
//!   program attaches to the parent terminal's console for this, since it is a
//!   GUI-subsystem executable on Windows.
//! * `--self-test`: open the GUI, drive it through a scripted check and exit
//!   with 0 (pass) or 1 (fail). Used by Windows CI.

// No console window on Windows; ignored on other targets.
#![windows_subsystem = "windows"]
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kalkan_config::{AppDirs, Config};
use kalkan_gui::{AppInfo, RunOptions};

const USAGE: &str = "\
KALKAN — Complex network technology. Simple user experience.

USAGE:
    kalkan [OPTIONS]

With no options, KALKAN opens its window.

OPTIONS:
    --config <PATH>          Use this configuration file instead of the default
    --check-config           Validate the configuration file and exit
    --print-default-config   Print the default configuration as TOML and exit
    --self-test              Open the window, run the scripted UI self-test, exit
    --screenshots <DIR>      With --self-test: save a BMP of each step to DIR
    -V, --version            Print version and exit
    -h, --help               Print this help and exit
";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    config: Option<PathBuf>,
    check_config: bool,
    print_default_config: bool,
    self_test: bool,
    screenshots: Option<PathBuf>,
    version: bool,
    help: bool,
}

impl Args {
    /// Whether this invocation prints to a terminal (vs. only opening the GUI).
    fn is_console_run(&self) -> bool {
        self.check_config
            || self.print_default_config
            || self.self_test
            || self.version
            || self.help
    }
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => {
                a.config = Some(PathBuf::from(it.next().ok_or("--config requires a path")?))
            }
            "--check-config" => a.check_config = true,
            "--print-default-config" => a.print_default_config = true,
            "--self-test" => a.self_test = true,
            "--screenshots" => {
                a.screenshots = Some(PathBuf::from(
                    it.next().ok_or("--screenshots requires a directory")?,
                ))
            }
            "-V" | "--version" => a.version = true,
            "-h" | "--help" => a.help = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.screenshots.is_some() && !a.self_test {
        return Err("--screenshots can only be used with --self-test".into());
    }
    Ok(a)
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if !raw.is_empty() {
        // Any argument may produce terminal output (including errors).
        kalkan_win::attach_parent_console();
    }
    let args = match parse_args(raw.into_iter()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    if args.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("kalkan {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.print_default_config {
        return match Config::default().to_toml_string() {
            Ok(s) => {
                print!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&args, &format!("{e}")),
        };
    }

    let config_path = match (&args.config, AppDirs::from_env()) {
        (Some(p), _) => p.clone(),
        (None, Some(d)) => d.config_file(),
        (None, None) => {
            return fail(
                &args,
                "cannot determine the configuration directory; pass --config <PATH>",
            )
        }
    };

    let config = match Config::load(&config_path) {
        Ok(c) => c,
        // Never fall back silently: the user must know their file is ignored.
        Err(e) => return fail(&args, &format!("{e}")),
    };

    if args.check_config {
        println!("{}: OK", config_path.display());
        return ExitCode::SUCCESS;
    }

    if let Err(e) = kalkan_logging::init(config.logging.level) {
        eprintln!("warning: {e}");
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), platform = %kalkan_win::platform_description(), "KALKAN starting");

    let info = app_info(&config, &config_path);
    tracing::info!(elevated = ?info.elevated, config = %config_path.display(), "start-up information gathered");

    let opts = RunOptions {
        start_minimized: config.ui.start_minimized,
        self_test: args.self_test,
        screenshot_dir: args.screenshots.clone(),
    };
    let engine = Box::new(kalkan_core::UnimplementedEngine::new());

    match kalkan_gui::run(info, engine, opts) {
        Ok(None) => {
            tracing::info!("KALKAN exited cleanly");
            ExitCode::SUCCESS
        }
        Ok(Some(report)) => print_self_test(&report),
        Err(e) => fail(&args, &format!("{e}")),
    }
}

/// Report a fatal error where the user will see it: the terminal for CLI
/// runs, a dialog box for normal GUI starts.
fn fail(args: &Args, message: &str) -> ExitCode {
    if args.is_console_run() {
        eprintln!("error: {message}");
    } else {
        kalkan_gui::show_error("KALKAN could not start", message);
    }
    ExitCode::FAILURE
}

fn yes_no(b: bool) -> String {
    if b { "Yes" } else { "No" }.to_owned()
}

/// Gather the real start-up facts the GUI displays.
fn app_info(config: &Config, config_path: &Path) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: kalkan_win::platform_description(),
        elevated: kalkan_win::is_elevated(),
        config_path: config_path.display().to_string(),
        config_file_exists: config_path.is_file(),
        settings: vec![
            (
                "Start engine on launch".into(),
                yes_no(config.general.start_engine_on_launch),
            ),
            (
                "Monitor sample interval".into(),
                format!("{} ms", config.monitor.sample_interval_ms),
            ),
            ("Log level".into(), config.logging.level.to_string()),
            (
                "File logging (Phase 5)".into(),
                yes_no(config.logging.file_logging),
            ),
            (
                "Retained log files".into(),
                config.logging.max_log_files.to_string(),
            ),
            ("Start minimized".into(), yes_no(config.ui.start_minimized)),
        ],
        log_level: config.logging.level.to_string(),
        engine_note: Some(
            "The engine runner arrives in Phase 3. Start and Stop are placeholders.".to_owned(),
        ),
    }
}

fn print_self_test(r: &kalkan_gui::SelfTestReport) -> ExitCode {
    println!("KALKAN GUI self-test");
    for c in &r.checks {
        println!("  [{}] {}", if c.passed { "PASS" } else { "FAIL" }, c.name);
    }
    for p in &r.screenshots {
        println!("  screenshot: {}", p.display());
    }
    for e in &r.screenshot_errors {
        println!("  screenshot error: {e}");
    }
    match kalkan_win::process_memory() {
        Some(m) => println!(
            "  memory after self-test: working set {:.1} MiB, peak {:.1} MiB, private {:.1} MiB",
            m.working_set as f64 / 1048576.0,
            m.peak_working_set as f64 / 1048576.0,
            m.private_bytes as f64 / 1048576.0
        ),
        None => println!("  memory: not available"),
    }
    let passed = r.passed();
    let failed = r.checks.iter().filter(|c| !c.passed).count();
    println!(
        "result: {} ({} checks, {} failed)",
        if passed { "PASS" } else { "FAIL" },
        r.checks.len(),
        failed
    );
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Args, String> {
        parse_args(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_opens_gui() {
        let a = args(&[]).unwrap();
        assert_eq!(a, Args::default());
        assert!(!a.is_console_run());
    }

    #[test]
    fn config_path_alone_is_a_gui_run() {
        let a = args(&["--config", "x.toml"]).unwrap();
        assert_eq!(a.config, Some(PathBuf::from("x.toml")));
        assert!(!a.is_console_run());
    }

    #[test]
    fn cli_flags_are_console_runs() {
        for f in [
            "--check-config",
            "--print-default-config",
            "--self-test",
            "-V",
            "--help",
        ] {
            assert!(args(&[f]).unwrap().is_console_run(), "{f}");
        }
    }

    #[test]
    fn self_test_with_screenshots() {
        let a = args(&["--self-test", "--screenshots", "out"]).unwrap();
        assert!(a.self_test);
        assert_eq!(a.screenshots, Some(PathBuf::from("out")));
    }

    #[test]
    fn errors() {
        assert!(args(&["--config"]).is_err());
        assert!(args(&["--frobnicate"]).is_err());
        assert!(
            args(&["--screenshots", "out"]).is_err(),
            "requires --self-test"
        );
        assert!(args(&["--self-test", "--screenshots"]).is_err());
    }

    #[test]
    fn app_info_reflects_config() {
        let mut c = Config::default();
        c.monitor.sample_interval_ms = 750;
        let i = app_info(&c, Path::new("definitely-missing.toml"));
        assert!(!i.config_file_exists);
        assert!(i
            .settings
            .iter()
            .any(|(k, v)| k == "Monitor sample interval" && v == "750 ms"));
        assert_eq!(i.log_level, "info");
    }
}
