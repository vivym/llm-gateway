use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::http::HeaderValue;
use clap::Parser;
use mimalloc::MiMalloc;
use thiserror::Error;
use tower_http::cors::AllowOrigin;

use common::logging::{init_logging, LogFormat, LogLevel};
use llm_gateway_server::server;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long, env, default_value = "0.0.0.0")]
    host: String,
    #[clap(long, short, env, default_value_t = 3000)]
    port: u16,
    #[clap(long, env)]
    cors_allow_origin: Option<Vec<String>>,
    #[clap(long, env)]
    client_token: Option<String>,
    #[clap(long, env)]
    num_threads: Option<usize>,
    #[clap(long, env)]
    otlp_endpoint: Option<String>,
    #[clap(long, env, default_value = "info")]
    log_level: LogLevel,
    #[clap(long, env, default_value = "text")]
    log_format: LogFormat,
    #[clap(long, env, default_value_t = true)]
    log_colorize: bool,
}

fn main() -> Result<(), LLMGatewayError> {
    match dotenvy::dotenv() {
        Ok(_) | Err(dotenvy::Error::Io(_)) => {}
        Err(e) => panic!("Failed to load .env file: {:?}", e),
    }

    let args = Args::parse();
    println!("Args: {:?}", args);
    let Args {
        host,
        port,
        cors_allow_origin,
        client_token,
        num_threads,
        otlp_endpoint,
        log_level,
        log_format,
        log_colorize,
    } = args;

    init_logging(
        env!("CARGO_PKG_NAME"),
        otlp_endpoint,
        log_level,
        log_format,
        log_colorize,
    );

    let client_token = client_token.unwrap_or_else(generate_client_token);
    if client_token.len() < 32 {
        tracing::warn!("Invalid client token length: {}", client_token.len());
        return Err(LLMGatewayError::WebServerError(
            server::WebServerError::InvalidClientToken,
        ));
    }

    tracing::info!("Client token: {}", client_token);

    let cors_allow_origin = cors_allow_origin.map(|allow_origin| {
        AllowOrigin::list(
            allow_origin
                .iter()
                .map(|origin| origin.parse::<HeaderValue>().unwrap()),
        )
    });

    let addr = match host.parse() {
        Ok(ip) => SocketAddr::new(ip, port),
        Err(_) => {
            tracing::warn!("Invalid hostname: `{}`, defaulting to `0.0.0.0`", host);
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port)
        }
    };

    let num_threads = num_threads.unwrap_or_else(|| num_cpus::get() * 2);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_threads)
        .enable_all()
        .build()?
        .block_on(server::run(addr, cors_allow_origin, client_token))?;

    Ok(())
}

fn generate_client_token() -> String {
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};

    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

#[derive(Debug, Error)]
enum LLMGatewayError {
    #[error("WebServer error: {0}")]
    WebServerError(#[from] server::WebServerError),
    #[error("Tokio runtime failed to start: {0}")]
    Tokio(#[from] std::io::Error),
}
