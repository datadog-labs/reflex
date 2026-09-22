//! Exercises the actual HTTP/protobuf exporters against a loopback intake, without credentials.
#[path = "../examples/support/datadog.rs"]
mod datadog;
mod support;
use opentelemetry::KeyValue;
use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
        trace::v1::ExportTraceServiceRequest,
    },
    metrics::v1::{metric::Data, AggregationTemporality},
};
use opentelemetry_sdk::Resource;
use prost::Message;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tracing::instrument::WithSubscriber;
use typesafe_ai::*;

#[test]
fn direct_datadog_export_sends_three_signals_with_delta_metrics_and_correlated_logs() {
    // Exercise the env constructor's type without reading credentials in tests.
    let _ = datadog::Telemetry::from_env;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let saved = requests.clone();
    let done = Arc::new(AtomicBool::new(false));
    let stop = done.clone();
    let intake = std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let (mut socket, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            let mut buf = [0; 4096];
            let end = loop {
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0);
                data.extend_from_slice(&buf[..n]);
                if let Some(i) = data.windows(4).position(|b| b == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let headers = String::from_utf8(data[..end].to_vec()).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while data.len() < end + length {
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0);
                data.extend_from_slice(&buf[..n]);
            }
            saved
                .lock()
                .unwrap()
                .push((headers, data[end..end + length].to_vec()));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        }
    });
    let telemetry = datadog::Telemetry::build(
        &endpoint,
        "fake-datadog-key".into(),
        Resource::builder()
            .with_service_name("typesafe-test")
            .with_attribute(KeyValue::new("deployment.environment.name", "test"))
            .build(),
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(
        async {
            let mut retry = support::Reply::json(429, json!({"error":"secret-provider-body"}));
            retry.headers = "Retry-After: 0\r\n".into();
            let server = support::Server::start(vec![
                retry,
                support::Reply::json(
                    200,
                    json!({
                        "model":"jev-test","usage":{"input_tokens":4,"output_tokens":1},
                        "answers":{"action":{"type":"choice","choice":"admit","confidence":0.8,
                            "probabilities":{"admit":0.9,"defer":0.1}}}
                    }),
                ),
            ])
            .await;
            let client = TypeSafeClient::builder()
                .api_key("secret-typesafe-key")
                .endpoint(&server.endpoint)
                .meter(telemetry.meter())
                .build()
                .unwrap();
            let task = SystemOneTask::builder()
                .model("jev-test")
                .questions(questions! {
                    action: choice("secret-question", [("admit","yes"),("defer","no")]),
                })
                .build()
                .unwrap();
            client
                .system_one(&task, &json!({"value":"secret-state"}))
                .await
                .unwrap();
            assert_eq!(server.requests().len(), 2);
            server.finish().await;
        }
        .with_subscriber(telemetry.dispatch.clone()),
    );
    let result = telemetry.shutdown();
    done.store(true, Ordering::Relaxed);
    intake.join().unwrap();
    result.unwrap();
    let requests = requests.lock().unwrap();
    let body_for = |path: &str| {
        let (headers, body) = requests
            .iter()
            .find(|(headers, _)| headers.starts_with(&format!("POST {path} ")))
            .unwrap();
        let lower = headers.to_lowercase();
        assert!(lower.contains("dd-api-key: fake-datadog-key"));
        assert!(lower.contains("content-type: application/x-protobuf"));
        for secret in [
            "secret-typesafe-key",
            "secret-state",
            "secret-question",
            "secret-provider-body",
            "fake-datadog-key",
        ] {
            assert!(!String::from_utf8_lossy(body).contains(secret));
        }
        (headers, body)
    };
    let (headers, body) = body_for("/v1/metrics");
    assert!(headers.contains("resource_attributes_as_tags"));
    let metrics = ExportMetricsServiceRequest::decode(body.as_slice()).unwrap();
    assert!(format!("{metrics:?}").contains("typesafe-test"));
    let metrics: Vec<_> = metrics
        .resource_metrics
        .iter()
        .flat_map(|r| &r.scope_metrics)
        .flat_map(|s| &s.metrics)
        .collect();
    assert!(metrics.iter().any(|m| m.name == "typesafe.client.requests"));
    assert!(metrics
        .iter()
        .any(|m| m.name == "typesafe.client.calls.in_flight"
            && matches!(&m.data, Some(Data::Gauge(_)))));
    for metric in metrics {
        match metric.data.as_ref().unwrap() {
            Data::Sum(s) => assert_eq!(
                s.aggregation_temporality,
                AggregationTemporality::Delta as i32
            ),
            Data::Histogram(h) => assert_eq!(
                h.aggregation_temporality,
                AggregationTemporality::Delta as i32
            ),
            Data::Gauge(_) => {}
            _ => panic!("unexpected metric type"),
        }
    }
    let (headers, body) = body_for("/v1/traces");
    assert!(headers.to_lowercase().contains("compute_stats: true"));
    let traces = ExportTraceServiceRequest::decode(body.as_slice()).unwrap();
    let spans: Vec<_> = traces
        .resource_spans
        .iter()
        .flat_map(|r| &r.scope_spans)
        .flat_map(|s| &s.spans)
        .collect();
    let root = spans
        .iter()
        .find(|s| s.name == "typesafe.system_one")
        .unwrap();
    assert_eq!(
        spans
            .iter()
            .filter(|s| s.name == "typesafe.http_attempt")
            .count(),
        2
    );
    let (_, body) = body_for("/v1/logs");
    let logs = ExportLogsServiceRequest::decode(body.as_slice()).unwrap();
    let records: Vec<_> = logs
        .resource_logs
        .iter()
        .flat_map(|r| &r.scope_logs)
        .flat_map(|s| &s.log_records)
        .collect();
    assert!(!records.is_empty());
    assert!(records
        .iter()
        .all(|l| l.trace_id == root.trace_id && l.span_id == root.span_id));
}
