use opentelemetry::KeyValue;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    trace::{self, Sampler},
    Resource,
};
use opentelemetry_otlp::WithExportConfig;
use serde::Serialize;
use tracing_subscriber::{
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
    Layer,
};

#[derive(clap::ValueEnum, Clone, Default, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]

pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl AsRef<str> for LogLevel {
    fn as_ref(&self) -> &str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

#[derive(clap::ValueEnum, Clone, Default, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

/// Initialize the logging system.
///     - otlp_endpoint: The OpenTelemetry collector endpoint.
///     - json_output: Whether to output logs in JSON format.
///     - log_level: The log level.
///     - log_format: The log format.
///     - log_colorize: Whether to colorize the log output.
pub fn init_logging(
    package_name: &'static str,
    otlp_endpoint: Option<String>,
    log_level: LogLevel,
    log_format: LogFormat,
    log_colorize: bool,
) {
    let mut layers = Vec::new();

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_ansi(log_colorize)
        .with_line_number(true);

    let fmt_layer = match log_format {
        LogFormat::Text => fmt_layer.boxed(),
        LogFormat::Json => fmt_layer.json().flatten_event(true).boxed(),
    };
    layers.push(fmt_layer);

    if let Some(otlp_endpoint) = otlp_endpoint {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(otlp_endpoint),
            )
            .with_trace_config(
                trace::config()
                    .with_resource(Resource::new(vec![KeyValue::new(
                        "service.name",
                        package_name,
                    )]))
                    .with_sampler(Sampler::AlwaysOn),
            )
            .install_batch(opentelemetry_sdk::runtime::Tokio);

        if let Ok(tracer) = tracer {
            layers.push(tracing_opentelemetry::layer().with_tracer(tracer).boxed());
            init_tracing_opentelemetry::init_propagator().unwrap();
        }
    }

    tracing_subscriber::registry()
        .with(EnvFilter::new(log_level))
        .with(layers)
        .init();
}
