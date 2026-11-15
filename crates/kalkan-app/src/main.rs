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
use std::sync::Arc;
use std::time::Duration;

mod netcli;

use kalkan_config::{AppDirs, Config};
use kalkan_gui::{AppInfo, RunOptions};
use kalkan_logging::Output;

const USAGE: &str = "\
KALKAN — Complex network technology. Simple user experience.

USAGE:
    kalkan [OPTIONS]

With no options, KALKAN opens its window.

OPTIONS:
    --config <PATH>          Use this configuration file instead of the default
    --check-config           Validate the configuration file and exit
    --print-default-config   Print the default configuration as TOML and exit
    --interfaces             List network interfaces and their counters, then exit
    --benchmark              Measure monitoring overhead (about 25 s), then exit
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
    interfaces: bool,
    benchmark: bool,
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
            || self.interfaces
            || self.benchmark
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
            "--interfaces" => a.interfaces = true,
            "--benchmark" => a.benchmark = true,
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
    if args.interfaces {
        return netcli::print_interfaces();
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
    if args.benchmark {
        return netcli::run_benchmark(&config, engine_spec(&config));
    }

    let log = init_logging(&args, &config);
    kalkan_logging::install_panic_hook();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), platform = %kalkan_win::platform_description(), "KALKAN starting");

    let mut info = app_info(&config, &config_path, &log);
    // The self-test drives the engine itself and must start from Stopped.
    info.start_engine_on_launch = config.general.start_engine_on_launch && !args.self_test;
    tracing::info!(elevated = ?info.elevated, config = %config_path.display(), "start-up information gathered");

    let opts = RunOptions {
        start_minimized: config.ui.start_minimized,
        self_test: args.self_test,
        screenshot_dir: args.screenshots.clone(),
        cpu_time: Some(kalkan_win::process_cpu_time),
    };
    let engine = match kalkan_core::EngineHandle::spawn(engine_spec(&config)) {
        Ok(h) => Box::new(h),
        Err(e) => return fail(&args, &format!("could not start the engine thread: {e}")),
    };

    // The GUI owns the engine; dropping it (when the window closes) joins the
    // engine threads.
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

/// Where log output goes, and what the Logs page says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LogChoice {
    output: Output,
    note: String,
}

/// Console runs log to the terminal. GUI runs log to a file, because a
/// GUI-subsystem process has no console and stderr would drop everything.
fn choose_log_output(args: &Args, config: &Config, dirs: Option<&AppDirs>) -> LogChoice {
    if args.is_console_run() {
        return LogChoice {
            output: Output::Stderr,
            note: "Console run: messages go to the terminal".into(),
        };
    }
    if !config.logging.file_logging {
        return LogChoice {
            output: Output::Stderr,
            note: "Disabled by logging.file_logging in kalkan.toml".into(),
        };
    }
    match dirs {
        Some(d) => LogChoice {
            output: Output::File(d.log_dir.join(kalkan_logging::LOG_FILE_NAME)),
            note: "Renamed to kalkan.log.old when larger than 5 MB".into(),
        },
        None => LogChoice {
            output: Output::Stderr,
            note: "The log directory could not be determined".into(),
        },
    }
}

fn init_logging(args: &Args, config: &Config) -> LogChoice {
    let mut choice = choose_log_output(args, config, AppDirs::from_env().as_ref());
    if let Err(e) = kalkan_logging::init(config.logging.level, &choice.output) {
        // Never lose this silently: fall back to stderr and say why.
        let _ = kalkan_logging::init(config.logging.level, &Output::Stderr);
        tracing::error!(%e, "file logging unavailable; using stderr");
        choice = LogChoice {
            output: Output::Stderr,
            note: format!("Log file unavailable: {e}"),
        };
    }
    choice
}

/// Real process metrics for the engine, read through Win32 calls.
struct WinProcessProbe;

impl kalkan_core::ProcessProbe for WinProcessProbe {
    fn sample(&mut self) -> Result<kalkan_core::ProcessSample, String> {
        let cpu_time = kalkan_win::process_cpu_time()
            .ok_or("GetProcessTimes is not available on this platform or failed")?;
        let memory = kalkan_win::process_memory()
            .ok_or("GetProcessMemoryInfo is not available on this platform or failed")?;
        Ok(kalkan_core::ProcessSample {
            cpu_time,
            memory_bytes: memory.working_set,
        })
    }
}

fn engine_spec(config: &Config) -> kalkan_core::EngineSpec {
    kalkan_core::EngineSpec {
        sample_interval: Duration::from_millis(config.monitor.sample_interval_ms.into()),
        probe: Arc::new(|| Ok(Box::new(WinProcessProbe) as Box<dyn kalkan_core::ProcessProbe>)),
        network: Some(Arc::new(|| {
            if cfg!(windows) {
                Ok(Box::new(kalkan_win::WinNetworkMonitor::new())
                    as Box<dyn kalkan_net::NetworkMonitor>)
            } else {
                Err("network monitoring is only implemented for Windows".into())
            }
        })),
    }
}

/// Gather the real start-up facts the GUI displays.
fn app_info(config: &Config, config_path: &Path, log: &LogChoice) -> AppInfo {
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
                "Sample interval".into(),
                format!("{} ms", config.monitor.sample_interval_ms),
            ),
            ("Log level".into(), config.logging.level.to_string()),
            ("File logging".into(), yes_no(config.logging.file_logging)),
            (
                "Retained log files".into(),
                config.logging.max_log_files.to_string(),
            ),
            ("Start minimized".into(), yes_no(config.ui.start_minimized)),
        ],
        log_level: config.logging.level.to_string(),
        log_file: match &log.output {
            Output::File(p) => Some(p.display().to_string()),
            Output::Stderr => None,
        },
        log_note: log.note.clone(),
        start_engine_on_launch: config.general.start_engine_on_launch,
        sample_interval_ms: config.monitor.sample_interval_ms,
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
            "--interfaces",
            "--benchmark",
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

    fn stderr_choice() -> LogChoice {
        LogChoice {
            output: Output::Stderr,
            note: "n".into(),
        }
    }

    #[test]
    fn app_info_reflects_config() {
        let mut c = Config::default();
        c.monitor.sample_interval_ms = 750;
        let i = app_info(&c, Path::new("definitely-missing.toml"), &stderr_choice());
        assert!(!i.config_file_exists);
        assert!(i
            .settings
            .iter()
            .any(|(k, v)| k == "Sample interval" && v == "750 ms"));
        assert_eq!(i.log_level, "info");
        assert_eq!(i.log_file, None);
        assert!(!i.start_engine_on_launch, "default is off");
    }

    #[test]
    fn engine_spec_uses_configured_interval() {
        let mut c = Config::default();
        c.monitor.sample_interval_ms = 2500;
        assert_eq!(engine_spec(&c).sample_interval, Duration::from_millis(2500));
    }

    #[test]
    fn gui_runs_log_to_a_file_console_runs_to_stderr() {
        let dirs = AppDirs {
            config_dir: PathBuf::from("cfg"),
            log_dir: PathBuf::from("logs"),
        };
        let c = Config::default();
        let gui = choose_log_output(&args(&[]).unwrap(), &c, Some(&dirs));
        assert_eq!(
            gui.output,
            Output::File(PathBuf::from("logs").join("kalkan.log"))
        );
        let cli = choose_log_output(&args(&["--self-test"]).unwrap(), &c, Some(&dirs));
        assert_eq!(cli.output, Output::Stderr);
        let mut off = Config::default();
        off.logging.file_logging = false;
        let disabled = choose_log_output(&args(&[]).unwrap(), &off, Some(&dirs));
        assert_eq!(disabled.output, Output::Stderr);
        assert!(disabled.note.contains("file_logging"));
        assert_eq!(
            choose_log_output(&args(&[]).unwrap(), &c, None).output,
            Output::Stderr
        );
    }

    #[test]
    fn engine_starts_stopped_and_shuts_down_cleanly_from_the_app_spec() {
        use kalkan_core::{Command, EngineLink, Event};
        let mut h = kalkan_core::EngineHandle::spawn(engine_spec(&Config::default())).unwrap();
        h.send(Command::QueryStatus);
        let ev = h.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            matches!(ev, Event::Status(s) if s.state == Some(kalkan_core::EngineState::Stopped))
        );
        h.shutdown(Duration::from_secs(10)).unwrap();
        assert_eq!(h.threads_alive(), 0);
    }
}
