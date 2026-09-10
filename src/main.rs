#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use dlna_rs::config::Config;
use dlna_rs::content::folder::FolderMirror;
use dlna_rs::core::http::HttpServer;
use dlna_rs::core::net;
use dlna_rs::core::ssdp::Ssdp;
use dlna_rs::scanner;
use uuid::Uuid;

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

fn init_logging(config: &dlna_rs::config::LoggingConfig) {
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

/// Resolves `server.uuid`: a fixed UUID if one was configured, otherwise a
/// fresh one generated for this run. Config validation already confirmed
/// this is either "auto" or a valid UUID, so parsing here can't fail.
fn resolve_uuid(configured: &str) -> Uuid {
    if configured.eq_ignore_ascii_case("auto") {
        Uuid::new_v4()
    } else {
        Uuid::parse_str(configured).expect("config validation already checked this parses")
    }
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

    let interface_addr = match net::interface_ipv4(&config.server.interface) {
        Ok(addr) => addr,
        Err(err) => {
            log::error!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let uuid = resolve_uuid(&config.server.uuid);
    let location = format!(
        "http://{interface_addr}:{port}/description.xml",
        port = config.server.port
    );
    log::info!("device uuid: {uuid}, location: {location}");

    let ssdp = match Ssdp::bind(interface_addr, uuid, location, config.ssdp.max_age()).await {
        Ok(ssdp) => ssdp,
        Err(err) => {
            log::error!("failed to bind SSDP socket: {err}");
            return ExitCode::FAILURE;
        }
    };

    let index = scanner::scan(&config.media);
    log::info!("scanned media directories: {} entries indexed", index.len());
    let content_source = FolderMirror::new(index);

    let http = match HttpServer::bind(
        interface_addr,
        config.server.port,
        config.server.friendly_name.clone(),
        uuid,
        content_source,
    )
    .await
    {
        Ok(http) => http,
        Err(err) => {
            log::error!("failed to bind HTTP server: {err}");
            return ExitCode::FAILURE;
        }
    };

    ssdp.announce_alive().await;
    log::info!("SSDP responder listening on 239.255.255.250:1900");
    log::info!(
        "HTTP server listening on {interface_addr}:{}",
        config.server.port
    );

    let responder = tokio::spawn({
        let ssdp = ssdp.clone();
        async move { ssdp.serve_search_requests().await }
    });
    let announcer = tokio::spawn({
        let ssdp = ssdp.clone();
        let interval = config.ssdp.notify_interval;
        async move { ssdp.announce_alive_periodically(interval).await }
    });
    let http_server = tokio::spawn(http.serve());

    wait_for_shutdown().await;

    responder.abort();
    announcer.abort();
    http_server.abort();
    ssdp.announce_byebye().await;
    log::info!("sent ssdp:byebye, exiting");

    ExitCode::SUCCESS
}
