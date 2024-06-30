use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::http::HeaderValue;
use clap::Parser;
use tower_http::cors::AllowOrigin;
use thiserror::Error;

use common::logging::{init_logging, LogLevel, LogFormat};
use llm_gateway_server::server;

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
    otlp_endpoint: Option<String>,
    #[clap(long, env, default_value = "info")]
    log_level: LogLevel,
    #[clap(long, env, default_value = "text")]
    log_format: LogFormat,
    #[clap(long, env, default_value_t = true)]
    log_colorize: bool,
}

#[tokio::main]
async fn main() -> Result<(), LLMGatewayError> {
    match dotenvy::dotenv() {
        Ok(_) | Err(dotenvy::Error::Io(_)) => {},
        Err(e) => panic!("Failed to load .env file: {:?}", e),
    }

    let args = Args::parse();
    println!("Args: {:?}", args);
    let Args {
        host,
        port,
        cors_allow_origin,
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

    let cors_allow_origin = cors_allow_origin.map(|allow_origin| {
        AllowOrigin::list(
            allow_origin
                .iter()
                .map(|origin| origin.parse::<HeaderValue>().unwrap())
        )
    });

    let addr = match host.parse() {
        Ok(ip) => SocketAddr::new(ip, port),
        Err(_) => {
            tracing::warn!("Invalid hostname: `{}`, defaulting to `0.0.0.0`", host);
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port)
        }
    };

    server::run(addr, cors_allow_origin).await?;

    Ok(())
}

#[derive(Debug, Error)]
enum LLMGatewayError {
    #[error("WebServer error: {0}")]
    WebServerError(#[from] server::WebServerError),
    #[error("Tokio runtime failed to start: {0}")]
    Tokio(#[from] std::io::Error),
}
