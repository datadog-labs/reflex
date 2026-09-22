//! Application-owned telemetry setup. The client itself never installs providers/exporters.
use opentelemetry::{
    metrics::{Meter, MeterProvider},
    trace::TracerProvider,
    KeyValue,
};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    logs::SdkLoggerProvider,
    metrics::{PeriodicReader, SdkMeterProvider, Temporality},
    trace::SdkTracerProvider,
    Resource,
};
use std::{collections::HashMap, error::Error, time::Duration};
use tracing_subscriber::{filter::Targets, prelude::*};

type SetupError = Box<dyn Error + Send + Sync>;
pub struct Telemetry {
    meters: SdkMeterProvider,
    traces: SdkTracerProvider,
    logs: SdkLoggerProvider,
    pub dispatch: tracing::Dispatch,
}
impl Telemetry {
    pub fn from_env() -> Result<Self, SetupError> {
        let key = std::env::var("DD_API_KEY").map_err(|_| "DD_API_KEY is required")?;
        if key.trim().is_empty() {
            return Err("DD_API_KEY must not be empty".into());
        }
        let site = std::env::var("DD_SITE").unwrap_or_else(|_| "datadoghq.com".into());
        if ![
            "datadoghq.com",
            "datadoghq.eu",
            "us3.datadoghq.com",
            "us5.datadoghq.com",
            "ap1.datadoghq.com",
            "ap2.datadoghq.com",
            "uk1.datadoghq.com",
        ]
        .contains(&site.as_str())
        {
            return Err(
                "DD_SITE is not a supported commercial Datadog site in this example".into(),
            );
        }
        let resource = Resource::builder()
            .with_service_name(
                std::env::var("DD_SERVICE").unwrap_or_else(|_| "typesafe-client-example".into()),
            )
            .with_attributes([
                KeyValue::new(
                    "deployment.environment.name",
                    std::env::var("DD_ENV").unwrap_or_else(|_| "local".into()),
                ),
                KeyValue::new(
                    "service.version",
                    std::env::var("DD_VERSION")
                        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into()),
                ),
            ])
            .build();
        Self::build(&format!("https://otlp.{site}"), key, resource)
    }

    // Explicit endpoint injection is used by the local wire-contract test, not exposed via env.
    pub fn build(base: &str, api_key: String, resource: Resource) -> Result<Self, SetupError> {
        let headers = HashMap::from([("dd-api-key".into(), api_key)]);
        let mut metric_headers = headers.clone();
        metric_headers.insert(
            "dd-otel-metric-config".into(),
            r#"{"resource_attributes_as_tags":true,"histograms":{"mode":"distributions"}}"#.into(),
        );
        let mut trace_headers = headers.clone();
        trace_headers.insert("compute_stats".into(), "true".into());
        // Blocking HTTP clients run on the SDK's background threads. Initialize outside Tokio.
        // Disable redirects so authentication cannot be forwarded to a different endpoint.
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?;
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_http_client(http.clone())
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .with_endpoint(format!("{base}/v1/metrics"))
            .with_headers(metric_headers)
            .with_timeout(Duration::from_secs(5))
            .with_temporality(Temporality::Delta)
            .build()?;
        let trace_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_http_client(http.clone())
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .with_endpoint(format!("{base}/v1/traces"))
            .with_headers(trace_headers)
            .with_timeout(Duration::from_secs(5))
            .build()?;
        let log_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_http_client(http)
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .with_endpoint(format!("{base}/v1/logs"))
            .with_headers(headers)
            .with_timeout(Duration::from_secs(5))
            .build()?;
        let meters = SdkMeterProvider::builder()
            .with_resource(resource.clone())
            .with_reader(
                PeriodicReader::builder(metric_exporter)
                    .with_interval(Duration::from_secs(10))
                    .build(),
            )
            .build();
        let traces = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(trace_exporter)
            .build();
        let logs = SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(log_exporter)
            .build();
        // Export only our instrumentation. In particular, exclude exporter HTTP client logs
        // to avoid recursive telemetry and accidental logging of transport details.
        let dispatch = tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(traces.tracer("typesafe-ai")))
                .with(OpenTelemetryTracingBridge::new(&logs))
                .with(
                    Targets::new()
                        .with_target("typesafe_ai", tracing::Level::INFO)
                        .with_target("reflex", tracing::Level::INFO)
                        .with_target("reflex_sim", tracing::Level::INFO),
                ),
        );
        Ok(Self {
            meters,
            traces,
            logs,
            dispatch,
        })
    }
    pub fn meter(&self) -> Meter {
        self.meters.meter("typesafe-ai")
    }
    /// Install application-wide providers before constructing clients or spawning tasks.
    #[allow(dead_code)] // Only needed by applications with independently spawned tasks.
    pub fn install_global(&self) -> Result<(), SetupError> {
        tracing::dispatcher::set_global_default(self.dispatch.clone())?;
        opentelemetry::global::set_meter_provider(self.meters.clone());
        Ok(())
    }
    /// Flush all three pipelines before exiting; export failures cannot alter a model result.
    pub fn shutdown(self) -> Result<(), SetupError> {
        let metrics = self.meters.shutdown();
        let traces = self.traces.shutdown();
        let logs = self.logs.shutdown();
        metrics?;
        traces?;
        logs?;
        Ok(())
    }
}
