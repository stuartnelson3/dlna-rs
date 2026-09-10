#![forbid(unsafe_code)]

mod config;

use std::path::PathBuf;
use std::process::ExitCode;

use config::Config;

struct Args {
    config_path: PathBuf,
    show_version: bool,
}

impl Args {
    fn parse() -> Result<Args, lexopt::Error> {
        use lexopt::prelude::*;

        let mut config_path = PathBuf::from("dlna-rs.toml");
        let mut show_version = false;
        let mut parser = lexopt::Parser::from_env();

        while let Some(arg) = parser.next()? {
            match arg {
                Short('c') | Long("config") => {
                    config_path = PathBuf::from(parser.value()?);
                }
                Long("version") => show_version = true,
                Short('h') | Long("help") => {
                    print_usage();
                    std::process::exit(0);
                }
                _ => return Err(arg.unexpected()),
            }
        }

        Ok(Args {
            config_path,
            show_version,
        })
    }
}

fn print_usage() {
    println!(
        "Usage: dlna-rs [--config <path>] [--version]\n\n\
         Options:\n  \
         -c, --config <path>  Path to the TOML config file (default: dlna-rs.toml)\n  \
         --version             Print the version and exit\n  \
         -h, --help            Print this message"
    );
}

fn init_logging(config: &config::LoggingConfig) {
    let mut builder = env_logger::Builder::new();
    let default_level: log::LevelFilter = config
        .level
        .parse()
        .expect("config validation already checked this parses");
    builder.filter_level(default_level);
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        builder.parse_filters(&rust_log);
    }
    builder.init();
}

async fn wait_for_shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => log::info!("received SIGTERM, shutting down"),
        _ = sigint.recv() => log::info!("received SIGINT, shutting down"),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match Args::parse() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{err}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    if args.show_version {
        println!("dlna-rs {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    let config = match Config::load(&args.config_path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!(
                "error loading config from {}: {err}",
                args.config_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    init_logging(&config.logging);
    log::info!("loaded config from {}", args.config_path.display());
    log::debug!("{config:#?}");

    wait_for_shutdown().await;

    ExitCode::SUCCESS
}
