use std::{
    env,
    fmt::Write,
    fs,
    io::{self, Write as IoWrite},
    mem,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use futures::{channel::oneshot, pin_mut, prelude::*};

use opentelemetry::sdk::trace::{BatchConfig, TracerProvider};
use tracing::{debug, error};

use cealn_client::{BuildRequest, Client, ClientOptions};
use cealn_core::{
    files::{workspace_file_exists_in, WellKnownFileError},
    trace_call_result,
    tracing::error_value,
};
use cealn_data::{Label, LabelBuf};
use io::LineWriter;
use tracing_subscriber::{prelude::*, EnvFilter};

use crate::console::Console;

const BAD_LOGGERS: &[(&str, &str)] = &[
    ("cranelift_codegen", "warn"),
    ("cranelift_wasm", "warn"),
    ("regalloc", "warn"),
];

pub fn init(debug: bool, is_server: bool) -> LoggingGuard {
    let rust_log_set = env::var_os("CEALN_LOG").is_some();

    let mut rust_log = match env::var("CEALN_LOG") {
        Ok(rust_log) => Some(rust_log),
        Err(env::VarError::NotPresent) => {
            if debug {
                Some("info,cealn=debug,cealn_server=debug,cealn_client=debug,cealn_runtime=debug".to_owned())
            } else {
                None
            }
        }
        Err(env::VarError::NotUnicode(_)) => {
            panic!("RUST_LOG environment variable format: invalid unicode");
        }
    };

    let registry = tracing_subscriber::registry();

    let stderr_subscriber = if let Some(mut rust_log) = rust_log {
        // Reduce logging levels of some very badly behaved dependencies (cranelift is particularly awful)
        for (name, default_level) in BAD_LOGGERS.iter() {
            if !rust_log.contains(name) {
                write!(&mut rust_log, ",{}={}", name, default_level).unwrap();
            }
        }

        // Set the environment variable so it propagates to children
        env::set_var("CEALN_LOG", &rust_log);

        let env_filter = match tracing_subscriber::EnvFilter::try_new(&rust_log) {
            Ok(env_filter) => env_filter,
            Err(err) => {
                panic!("error in RUST_LOG environment variable format: {}", err);
            }
        };

        Some(
            tracing_subscriber::fmt::layer()
                // If we are asking for any trace-level logs, print full span events. Otherwise only print span activation.
                .with_span_events(if rust_log.contains("trace") {
                    tracing_subscriber::fmt::format::FmtSpan::FULL
                } else {
                    tracing_subscriber::fmt::format::FmtSpan::CLOSE
                })
                .with_target(true)
                .with_file(true)
                .with_line_number(true)
                .with_ansi(!is_server)
                .with_writer(|| LineWriter::new(io::stderr()))
                .with_filter(env_filter),
        )
    } else {
        None
    };

    let subscriber = registry.with(stderr_subscriber);

    let provider;
    let subscriber = {
        use opentelemetry_otlp::WithExportConfig;

        let subscriber = {
            let otlp_exporter = opentelemetry_otlp::new_exporter()
                .http()
                .with_http_client(reqwest::Client::builder().build().unwrap())
                .with_endpoint("http://otel.dev.hardscience.io/v1/traces");
            let tracer = opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_exporter(otlp_exporter)
                .with_trace_config(opentelemetry::sdk::trace::config().with_resource(
                    opentelemetry::sdk::Resource::new(vec![
                        opentelemetry::KeyValue::new(
                            opentelemetry_semantic_conventions::resource::SERVICE_NAME.as_str(),
                            "cealn",
                        ),
                        opentelemetry::KeyValue::new("build.profile", env!("PROFILE")),
                    ]),
                ))
                .with_batch_config(
                    BatchConfig::default()
                        .with_max_queue_size(1024 * 1024)
                        .with_max_concurrent_exports(16),
                )
                .install_batch(opentelemetry::runtime::Tokio)
                .unwrap();
            provider = tracer.provider().unwrap();

            let telemetry = tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(EnvFilter::new("debug"));
            subscriber.with(telemetry)
        };
        subscriber
    };

    tracing::subscriber::set_global_default(subscriber).unwrap();

    LoggingGuard {
        provider: Some(provider),
    }
}

pub struct LoggingGuard {
    provider: Option<TracerProvider>,
}

impl LoggingGuard {
    pub async fn flush(mut self) {
        let _ = tokio::task::spawn_blocking({
            let provider = self.provider.take().unwrap();
            move || {
                provider.force_flush();
                opentelemetry::global::shutdown_tracer_provider();
            }
        })
        .await;
    }
}
