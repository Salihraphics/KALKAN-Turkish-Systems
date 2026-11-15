//! KALKAN entry point.
//!
//! Phase 1: a console program that wires the modules together, loads and
//! validates configuration, initialises logging and reports platform status.
//! It performs no network activity and starts no background threads.
//!
//! Phase 2 switches this binary to the Windows GUI subsystem and hands control
//! to `kalkan-gui`.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use kalkan_config::{AppDirs, Config};

const USAGE: &str = "\
KALKAN — Complex network technology. Simple user experience.

USAGE:
    kalkan [OPTIONS]

OPTIONS:
    --config <PATH>          Use this configuration file instead of the default
    --check-config           Validate the configuration file and exit
    --print-default-config   Print the default configuration as TOML and exit
    -V, --version            Print version and exit
    -h, --help               Print this help and exit
";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    config: Option<PathBuf>,
    check_config: bool,
    print_default_config: bool,
    version: bool,
    help: bool,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => {
                let p = it.next().ok_or("--config requires a path")?;
                a.config = Some(PathBuf::from(p));
            }
            "--check-config" => a.check_config = true,
            "--print-default-config" => a.print_default_config = true,
            "-V" | "--version" => a.version = true,
            "-h" | "--help" => a.help = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
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
        match Config::default().to_toml_string() {
            Ok(s) => {
                print!("{s}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let dirs = AppDirs::from_env();
    let config_path = match (&args.config, &dirs) {
        (Some(p), _) => p.clone(),
        (None, Some(d)) => d.config_file(),
        (None, None) => {
            eprintln!("error: cannot determine the configuration directory; pass --config <PATH>");
            return ExitCode::FAILURE;
        }
    };

    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            // Do not fall back silently: the user must know their file is ignored.
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if args.check_config {
        println!("{}: OK", config_path.display());
        return ExitCode::SUCCESS;
    }

    if let Err(e) = kalkan_logging::init(config.logging.level) {
        eprintln!("warning: {e}");
    }

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "KALKAN starting");
    tracing::info!(platform = %kalkan_win::platform_description(), "platform");
    match kalkan_win::is_elevated() {
        Some(true) => tracing::info!("running with administrator privileges"),
        Some(false) => tracing::info!("running without administrator privileges"),
        None => tracing::info!("administrator privilege status unavailable"),
    }
    tracing::info!(path = %config_path.display(), "configuration loaded");

    let lifecycle = kalkan_core::Lifecycle::new();
    tracing::info!(state = %lifecycle.state(), "engine initialised (lifecycle runner: Phase 3)");
    tracing::info!(
        implemented = kalkan_gui::IMPLEMENTED,
        pages = ?kalkan_gui::PAGES,
        "GUI (Phase 2)"
    );
    tracing::info!("KALKAN exiting cleanly");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Args, String> {
        parse_args(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args() {
        assert_eq!(args(&[]).unwrap(), Args::default());
    }

    #[test]
    fn config_path() {
        let a = args(&["--config", "x.toml", "--check-config"]).unwrap();
        assert_eq!(a.config, Some(PathBuf::from("x.toml")));
        assert!(a.check_config);
    }

    #[test]
    fn errors() {
        assert!(args(&["--config"]).is_err());
        assert!(args(&["--frobnicate"]).is_err());
    }
}
