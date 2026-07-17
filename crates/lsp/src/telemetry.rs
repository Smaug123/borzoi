//! Optional OpenTelemetry export, compiled only under the `otel` feature. Both
//! signals share one OTLP/HTTP transport:
//!
//! - **spans → Tempo**, from the `tracing` spans entered across the request
//!   handlers;
//! - **logs → Loki**, from `tracing` events (see the [`log_info!`](crate::log_info)
//!   family in [`crate::logging`]), bridged into OpenTelemetry log records.
//!
//! # Usage
//!
//! Build with the feature on, point it at your OTLP/HTTP collector (the
//! `grafana/otel-lgtm` image listens on `:4318` and fans the two signals out to
//! Tempo and Loki respectively), and run as usual:
//!
//! ```sh
//! cargo build --features otel
//! OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
//!     ./target/debug/borzoi
//! ```
//!
//! Everything is published under service name `borzoi`. The endpoint
//! defaults to `http://localhost:4318` when the env var is unset; set
//! `RUST_LOG` to control verbosity (default `info`). Because the log bridge
//! captures the active span's context, Loki log lines carry `TraceId`/`SpanId`
//! and link back to the matching Tempo trace in Grafana.
//!
//! Telemetry is exported over the network only: stdout stays reserved for the
//! LSP JSON-RPC stream and console logging goes to stderr, so enabling this
//! never corrupts the protocol.

/// Guard returned by [`init`]; flushes buffered spans on drop. Without the
/// `otel` feature it is an empty placeholder so the call site is identical.
#[cfg(not(feature = "otel"))]
pub struct Guard;

/// Initialise telemetry. Without the `otel` feature this is a no-op.
#[cfg(not(feature = "otel"))]
pub fn init() -> Guard {
    Guard
}

#[cfg(feature = "otel")]
pub use otel::{Guard, init};

#[cfg(feature = "otel")]
mod otel {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::logs::SdkLoggerProvider;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    /// Flushes and shuts down the tracer and logger providers on drop, so the
    /// spans and logs buffered by their batch processors reach the collector
    /// before the process exits.
    pub struct Guard {
        tracer_provider: SdkTracerProvider,
        logger_provider: SdkLoggerProvider,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            if let Err(err) = self.tracer_provider.shutdown() {
                eprintln!("borzoi: otel tracer shutdown failed: {err}");
            }
            if let Err(err) = self.logger_provider.shutdown() {
                eprintln!("borzoi: otel logger shutdown failed: {err}");
            }
        }
    }

    /// Build the OTLP/HTTP pipelines (spans → Tempo, logs → Loki) and install
    /// them as the global tracing subscriber. The endpoint is read from
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` (default `http://localhost:4318`). The
    /// blocking reqwest client lets our code stay synchronous (reqwest manages
    /// its own runtime internally), and each batch processor runs on its own
    /// thread so the stdio loop never blocks on the network.
    pub fn init() -> Guard {
        let resource = Resource::builder().with_service_name("borzoi").build();

        // Spans → Tempo.
        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .build()
            .expect("build OTLP span exporter");
        let tracer_provider = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(span_exporter)
            .build();
        let tracer = tracer_provider.tracer("borzoi");
        let otel_trace_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        // Logs → Loki. The appender bridges `tracing` events into OTel log
        // records, attaching the active span's trace context for correlation.
        let log_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .build()
            .expect("build OTLP log exporter");
        let logger_provider = SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(log_exporter)
            .build();
        // The bridge ships events to the collector; exporting an event makes
        // the HTTP client (reqwest/hyper) and the OTel SDK emit their own
        // `tracing` events, which would feed straight back through the bridge.
        // Deny those targets *on this layer only* so the loop can't form — the
        // stderr fmt layer below still shows them for local debugging.
        let otel_internal = Targets::new()
            .with_default(LevelFilter::TRACE)
            .with_target("opentelemetry", LevelFilter::OFF)
            .with_target("hyper", LevelFilter::OFF)
            .with_target("hyper_util", LevelFilter::OFF)
            .with_target("reqwest", LevelFilter::OFF)
            .with_target("h2", LevelFilter::OFF)
            .with_target("tonic", LevelFilter::OFF)
            .with_target("tower", LevelFilter::OFF);
        let otel_log_layer =
            OpenTelemetryTracingBridge::new(&logger_provider).with_filter(otel_internal);

        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        // fmt layer writes to STDERR — never stdout, which is the LSP wire.
        let fmt_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(otel_trace_layer)
            .with(otel_log_layer)
            .init();

        Guard {
            tracer_provider,
            logger_provider,
        }
    }
}
