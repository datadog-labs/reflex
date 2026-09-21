mod support;
use opentelemetry::{metrics::MeterProvider, trace::TracerProvider, KeyValue};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::{
    logs::{InMemoryLogExporter, SdkLoggerProvider},
    metrics::{
        data::{AggregatedMetrics, MetricData},
        InMemoryMetricExporter, InMemoryMetricExporterBuilder, PeriodicReader, SdkMeterProvider,
        Temporality,
    },
    trace::{InMemorySpanExporter, SdkTracerProvider},
};
use serde::{Serialize, Serializer};
use serde_json::json;
use std::time::Duration;
use support::{Reply, Server};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::{filter::Targets, prelude::*};
use typesafe_ai::*;

struct Capture {
    metrics: InMemoryMetricExporter,
    meter: SdkMeterProvider,
    spans: InMemorySpanExporter,
    tracer: SdkTracerProvider,
    logs: InMemoryLogExporter,
    logger: SdkLoggerProvider,
    dispatch: tracing::Dispatch,
}
impl Capture {
    fn new() -> Self {
        let metrics = InMemoryMetricExporterBuilder::new()
            .with_temporality(Temporality::Delta)
            .build();
        let meter = SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(metrics.clone())
                    .with_interval(Duration::from_secs(3600))
                    .build(),
            )
            .build();
        let spans = InMemorySpanExporter::default();
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(spans.clone())
            .build();
        let logs = InMemoryLogExporter::default();
        let logger = SdkLoggerProvider::builder()
            .with_simple_exporter(logs.clone())
            .build();
        let dispatch = tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(tracer.tracer("test")))
                .with(OpenTelemetryTracingBridge::new(&logger))
                .with(Targets::new().with_target("typesafe_ai", tracing::Level::TRACE)),
        );
        Self {
            metrics,
            meter,
            spans,
            tracer,
            logs,
            logger,
            dispatch,
        }
    }
    fn client(&self, endpoint: &str, timeout: Duration, retries: u32) -> TypeSafeClient {
        TypeSafeClient::builder()
            .api_key("secret-api-key")
            .endpoint(endpoint)
            .timeout(timeout)
            .max_retries(retries)
            .meter(self.meter.meter("typesafe-ai"))
            .build()
            .unwrap()
    }
    fn points(&self, name: &str) -> Vec<(Vec<KeyValue>, u64, f64)> {
        self.meter.force_flush().unwrap();
        self.metrics
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .filter(|m| m.name() == name)
            .flat_map(|m| match m.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                    assert_eq!(sum.temporality(), Temporality::Delta);
                    sum.data_points()
                        .map(|p| (p.attributes().cloned().collect(), p.value(), 0.0))
                        .collect::<Vec<_>>()
                }
                AggregatedMetrics::U64(MetricData::Gauge(g)) => g
                    .data_points()
                    .map(|p| (p.attributes().cloned().collect(), p.value(), 0.0))
                    .collect(),
                AggregatedMetrics::F64(MetricData::Histogram(h)) => {
                    assert_eq!(h.temporality(), Temporality::Delta);
                    h.data_points()
                        .map(|p| (p.attributes().cloned().collect(), p.count(), p.sum()))
                        .collect()
                }
                data => panic!("unexpected data: {data:?}"),
            })
            .collect()
    }
    fn count(&self, name: &str, status: &str) -> u64 {
        self.points(name)
            .iter()
            .filter(|(attrs, _, _)| attr(attrs, "status").as_deref() == Some(status))
            .map(|(_, count, _)| count)
            .sum()
    }
    fn assert_private(&self) {
        let output = format!(
            "{:?}{:?}{:?}",
            self.metrics.get_finished_metrics().unwrap(),
            self.spans.get_finished_spans().unwrap(),
            self.logs.get_emitted_logs().unwrap()
        );
        for secret in [
            "secret-api-key",
            "secret-state",
            "secret-question",
            "secret-answer",
            "secret-error",
        ] {
            assert!(!output.contains(secret), "telemetry exposed {secret}");
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.meter.shutdown().unwrap();
        self.tracer.shutdown().unwrap();
        self.logger.shutdown().unwrap();
    }
}
fn attr(attrs: &[KeyValue], key: &str) -> Option<String> {
    attrs
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.to_string())
}
macro_rules! task {
    () => {
        SystemOneTask::builder()
            .model("jev-alias")
            .questions(questions! {
                action: choice("secret-question", [("secret-answer", "yes"), ("wait", "no")]),
            })
            .build()
            .unwrap()
    };
}
fn response() -> serde_json::Value {
    json!({"model":"jev-resolved", "usage":{"input_tokens":12,"output_tokens":3},
        "answers":{"action":{"type":"choice","choice":"secret-answer","confidence":0.8,
            "probabilities":{"secret-answer":0.9,"wait":0.1}}}})
}

// Kept together so process-wide in-flight assertions do not race other telemetry cases.
#[tokio::test]
async fn lifecycle_metrics_traces_logs_and_privacy() {
    let c = Capture::new();
    let mut retry = Reply::json(429, json!({"error":"secret-error"}));
    retry.headers = "Retry-After: 0.02\r\nX-Request-Id: first-id\r\n".into();
    let mut ok = Reply::json(200, response());
    ok.headers = "X-Request-Id: final-id\r\n".into();
    let server = Server::start(vec![retry, ok]).await;
    let client = c.client(&server.endpoint, Duration::from_secs(2), 2);
    client
        .system_one(&task!(), &json!({"private":"secret-state"}))
        .with_subscriber(c.dispatch.clone())
        .await
        .unwrap();
    assert_eq!(server.requests().len(), 2);
    assert_eq!(c.count("typesafe.client.requests", "http_error"), 1);
    assert_eq!(c.count("typesafe.client.requests", "success"), 1);
    assert_eq!(c.count("typesafe.client.call.duration", "success"), 1);
    let requests = c.points("typesafe.client.requests");
    assert!(requests
        .iter()
        .any(|(a, _, _)| attr(a, "retry").as_deref() == Some("true")
            && attr(a, "http_status").as_deref() == Some("200")));
    let total: f64 = c
        .points("typesafe.client.call.duration")
        .iter()
        .map(|(_, _, s)| s)
        .sum();
    let attempts: f64 = c
        .points("typesafe.client.request.duration")
        .iter()
        .map(|(_, _, s)| s)
        .sum();
    let backoff: f64 = c
        .points("typesafe.client.retry.backoff.duration")
        .iter()
        .map(|(_, _, s)| s)
        .sum();
    assert!(backoff >= 0.02 && total >= attempts + backoff);
    let tokens = c.points("typesafe.client.tokens");
    assert_eq!(tokens.iter().map(|(_, n, _)| n).sum::<u64>(), 15);
    assert!(tokens
        .iter()
        .all(|(a, _, _)| attr(a, "model").as_deref() == Some("jev-resolved")));
    assert_eq!(
        c.points("typesafe.client.calls.in_flight")
            .last()
            .unwrap()
            .1,
        0
    );
    let spans = c.spans.get_finished_spans().unwrap();
    let call = spans
        .iter()
        .find(|s| s.name == "typesafe.system_one")
        .unwrap();
    let children: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "typesafe.http_attempt")
        .collect();
    assert_eq!(children.len(), 2);
    assert!(children
        .iter()
        .all(|s| s.parent_span_id == call.span_context.span_id()
            && s.span_context.trace_id() == call.span_context.trace_id()));
    assert_eq!(
        attr(&call.attributes, "request_id").as_deref(),
        Some("final-id")
    );
    let logs = c.logs.get_emitted_logs().unwrap();
    assert!(!logs.is_empty());
    assert!(logs
        .iter()
        .all(|l| l.record.trace_context().unwrap().trace_id == call.span_context.trace_id()));
    c.assert_private();
    server.finish().await;
    drop(c);

    // A typed answer error is a failed call even with HTTP 200. Valid usage is retained.
    let c = Capture::new();
    let mut bad = response();
    bad["answers"]["action"]["choice"] = json!("unknown");
    let server = Server::start(vec![Reply::json(200, bad)]).await;
    let result = c
        .client(&server.endpoint, Duration::from_secs(2), 2)
        .system_one(&task!(), &json!({}))
        .with_subscriber(c.dispatch.clone())
        .await;
    assert!(matches!(result, Err(Error::InvalidResponse(_))));
    assert_eq!(c.count("typesafe.client.requests", "invalid_response"), 1);
    assert_eq!(
        c.count("typesafe.client.call.duration", "invalid_response"),
        1
    );
    assert_eq!(
        c.points("typesafe.client.tokens")
            .iter()
            .map(|(_, n, _)| n)
            .sum::<u64>(),
        15
    );
    c.assert_private();
    server.finish().await;
    drop(c);

    // Preparation failure does not fabricate an HTTP request.
    let c = Capture::new();
    let client = c.client("http://127.0.0.1:1", Duration::from_secs(2), 0);
    client
        .system_one(&task!(), &42)
        .with_subscriber(c.dispatch.clone())
        .await
        .unwrap_err();
    struct BadState;
    impl Serialize for BadState {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("secret-error"))
        }
    }
    client
        .system_one(&task!(), &BadState)
        .with_subscriber(c.dispatch.clone())
        .await
        .unwrap_err();
    assert_eq!(
        c.count("typesafe.client.call.duration", "configuration_error"),
        1
    );
    assert_eq!(
        c.count("typesafe.client.call.duration", "serialization_error"),
        1
    );
    assert!(c.points("typesafe.client.requests").is_empty());
    c.assert_private();
    drop(c);

    // Deadline during backoff: the HTTP attempt stays an HTTP error, not a second timeout attempt.
    let c = Capture::new();
    let mut retry = Reply::json(503, json!({}));
    retry.headers = "Retry-After: 60\r\n".into();
    let server = Server::start(vec![retry]).await;
    let result = c
        .client(&server.endpoint, Duration::from_millis(80), 2)
        .system_one(&task!(), &json!({}))
        .with_subscriber(c.dispatch.clone())
        .await;
    assert!(matches!(result, Err(Error::Timeout)));
    assert_eq!(c.count("typesafe.client.requests", "http_error"), 1);
    assert_eq!(c.count("typesafe.client.requests", "timeout"), 0);
    assert_eq!(c.count("typesafe.client.call.duration", "timeout"), 1);
    assert_eq!(
        c.count("typesafe.client.retry.backoff.duration", "timeout"),
        1
    );
    let spans = c.spans.get_finished_spans().unwrap();
    let call = spans
        .iter()
        .find(|s| s.name == "typesafe.system_one")
        .unwrap();
    assert_eq!(
        attr(&call.attributes, "error.stage").as_deref(),
        Some("backing_off")
    );
    server.finish().await;
    drop(c);

    // Cancellation and the client's deadline while sending must both release in-flight accounting.
    for cancel in [false, true] {
        let c = Capture::new();
        let mut delayed = Reply::json(200, response());
        delayed.delay = Duration::from_secs(60);
        let server = Server::start(vec![delayed]).await;
        let client = c.client(&server.endpoint, Duration::from_millis(80), 0);
        let task = task!();
        let state = json!({});
        if cancel {
            let mut future = Box::pin(
                client
                    .system_one(&task, &state)
                    .with_subscriber(c.dispatch.clone()),
            );
            tokio::select! {
                result = &mut future => panic!("unexpected completion: {result:?}"),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
            assert_eq!(
                c.points("typesafe.client.calls.in_flight")
                    .last()
                    .unwrap()
                    .1,
                1
            );
            drop(future);
        } else {
            assert!(matches!(
                client
                    .system_one(&task, &state)
                    .with_subscriber(c.dispatch.clone())
                    .await,
                Err(Error::Timeout)
            ));
        }
        let status = if cancel { "cancelled" } else { "timeout" };
        assert_eq!(c.count("typesafe.client.requests", status), 1);
        assert_eq!(c.count("typesafe.client.call.duration", status), 1);
        assert_eq!(
            c.points("typesafe.client.calls.in_flight")
                .last()
                .unwrap()
                .1,
            0
        );
        c.assert_private();
    }

    // Independent clients and clones contribute to one process gauge. Unpolled futures do not.
    let c = Capture::new();
    let mut delayed = Reply::json(200, response());
    delayed.delay = Duration::from_secs(60);
    let server = Server::start(vec![delayed]).await;
    let client = c.client(&server.endpoint, Duration::from_secs(2), 0);
    let second = c.client(&server.endpoint, Duration::from_secs(2), 0);
    let cloned = client.clone();
    let task = task!();
    let state = json!({});
    drop(client.system_one(&task, &state));
    assert!(c.points("typesafe.client.call.duration").is_empty());
    let mut a = Box::pin(
        client
            .system_one(&task, &state)
            .with_subscriber(c.dispatch.clone()),
    );
    let mut b = Box::pin(
        second
            .system_one(&task, &state)
            .with_subscriber(c.dispatch.clone()),
    );
    let mut d = Box::pin(
        cloned
            .system_one(&task, &state)
            .with_subscriber(c.dispatch.clone()),
    );
    tokio::select! {
        _ = &mut a => panic!("unexpected completion"),
        _ = &mut b => panic!("unexpected completion"),
        _ = &mut d => panic!("unexpected completion"),
        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
    }
    assert_eq!(
        c.points("typesafe.client.calls.in_flight")
            .last()
            .unwrap()
            .1,
        3
    );
    drop((a, b, d));
    assert_eq!(
        c.points("typesafe.client.calls.in_flight")
            .last()
            .unwrap()
            .1,
        0
    );
    assert_eq!(c.count("typesafe.client.call.duration", "cancelled"), 3);
    assert_eq!(c.count("typesafe.client.requests", "cancelled"), 3);
    // Cancellation outside with_subscriber's polling scope must still close spans
    // and emit correlated terminal logs under the original dispatcher.
    let spans = c.spans.get_finished_spans().unwrap();
    let ids: std::collections::HashSet<_> = spans
        .iter()
        .filter(|s| s.name == "typesafe.system_one")
        .map(|s| s.span_context.trace_id())
        .collect();
    assert_eq!(ids.len(), 3);
    let terminal_logs = c.logs.get_emitted_logs().unwrap();
    assert_eq!(
        terminal_logs
            .iter()
            .filter(|l| l
                .record
                .attributes_iter()
                .any(|(key, value)| key.as_str() == "status"
                    && format!("{value:?}").contains("cancelled")))
            .count(),
        3
    );
    drop(c);

    // Transport failure and a terminal HTTP response are classified separately; no retries for 401.
    for status in [None, Some(401), Some(503)] {
        let c = Capture::new();
        let server = match status {
            Some(status) => Some(Server::start(vec![Reply::json(status, json!({}))]).await),
            None => None,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let endpoint = server
            .as_ref()
            .map_or(closed.as_str(), |s| s.endpoint.as_str());
        let client = c.client(endpoint, Duration::from_secs(2), 0);
        client
            .system_one(&task!(), &json!({}))
            .with_subscriber(c.dispatch.clone())
            .await
            .unwrap_err();
        let expected = if status.is_some() {
            "http_error"
        } else {
            "transport_error"
        };
        assert_eq!(c.count("typesafe.client.requests", expected), 1);
        assert_eq!(c.count("typesafe.client.call.duration", expected), 1);
        if status == Some(503) {
            let spans = c.spans.get_finished_spans().unwrap();
            let call = spans
                .iter()
                .find(|s| s.name == "typesafe.system_one")
                .unwrap();
            assert_eq!(
                attr(&call.attributes, "retries_exhausted").as_deref(),
                Some("true")
            );
        }
        c.assert_private();
        if let Some(server) = server {
            server.finish().await;
        }
    }
}
