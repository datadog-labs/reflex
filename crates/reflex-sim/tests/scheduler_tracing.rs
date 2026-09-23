// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use opentelemetry::metrics::MeterProvider;
use reflex_sim::{
    playground::inference::JevSettings,
    scheduler::{ClientConfig, Command, Session},
};
use std::{sync::Arc, time::Duration};
#[allow(dead_code)]
#[path = "support/metrics.rs"]
mod metrics;
use metrics::{count, Capture};
fn config(rate: f64, cpu: u32, duration_ms: u64) -> ClientConfig {
    ClientConfig {
        rate,
        cpu,
        memory_gib: cpu * 2,
        duration_ms,
        enabled: true,
    }
}
async fn set(session: &mut Session, id: u64, value: ClientConfig) {
    session
        .command(Command::Client { id, config: value })
        .await
        .unwrap();
}
async fn steps(session: &mut Session, count: usize) {
    for _ in 0..count {
        session.command(Command::Step).await.unwrap();
    }
}
#[test]
fn jev_trace_and_placement_outcomes_survive_background_handoff() {
    use axum::{routing::post, Json, Router};
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::prelude::*;
    let spans = InMemorySpanExporter::default();
    let tracer = SdkTracerProvider::builder()
        .with_simple_exporter(spans.clone())
        .build();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(tracer.tracer("scheduler-test"))),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for (cpu, fail, expected) in [
        (1, false, "placed"),
        (16, false, "evaluation_error"), // A node not offered by the task fails decoding.
        (1, false, "rejected"),          // A valid response can still fail the freshness guard.
        (1, true, "evaluation_error"),
    ] {
        spans.reset();
        let capture = Capture::new();
        runtime.block_on(async {
            let app = Router::new().route("/systemone", post(move || async move {
                if fail { (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"unavailable"}))) }
                else { (axum::http::StatusCode::OK, Json(serde_json::json!({"model":"jev-test","usage":{"input_tokens":5,"output_tokens":1},
                    "answers":{"placement":{"type":"choice","choice":"node_a","probabilities":{"node_a":1.0,"node_b":0.0,"node_c":0.0,"node_d":0.0,"defer":0.0}}}}))) }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/systemone", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
            let client = typesafe_ai::TypeSafeClient::builder().api_key("test-key").endpoint(endpoint).max_retries(0).build().unwrap();
            let judge = Arc::new(reflex_sim::scheduler::judge::LiveEvaluator::new(client, "jev-test".into()));
            let mut s = Session::with_meter(42, Some(judge), JevSettings { dispatch_interval: Duration::from_secs(60), ..Default::default() }, capture.provider.meter("scheduler")).unwrap();
            // Keep the mock's node-only response valid by offering one client head.
            set(&mut s, 1, config(0.0, 1, 1000)).await;
            set(&mut s, 2, config(0.0, 1, 1000)).await;
            set(&mut s, 0, config(1.0, cpu, 1000)).await;
            steps(&mut s, 1).await;
            tokio::time::timeout(Duration::from_secs(3), async {
                while !spans.get_finished_spans().unwrap().iter().any(|s| s.name == "scheduler.evaluate") {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap_or_else(|_| panic!("case={expected} calls={} pending={:?} spans={:?}", s.view().calls, s.view().pending_job, spans.get_finished_spans().unwrap().iter().map(|s| s.name.clone()).collect::<Vec<_>>()));
            assert!(s.view().decisions.is_empty());
            assert!(!spans.get_finished_spans().unwrap().iter().any(|s| s.name == "scheduler.decision"));
            // The finished evaluation span is exported before its task closes. Yield so the
            // clock can consume a completed task, exactly as it does on the next real tick.
            tokio::time::sleep(Duration::from_millis(2)).await;
            if expected == "rejected" {
                s.command(Command::Play).await.unwrap();
                s.tick(6000).await.unwrap();
            } else { steps(&mut s, 1).await; }
            assert_eq!(s.view().decisions[0].status, expected);
            assert_eq!(count(&capture.read(), "scheduler.placements", &[("outcome", expected)]), 1);
            if expected != "placed" { assert_eq!(count(&capture.read(), "scheduler.jobs", &[]), 0); }
            server.abort();
        }.with_subscriber(dispatch.clone()));
        tracer.force_flush().unwrap();
        let exported = spans.get_finished_spans().unwrap();
        let root = exported
            .iter()
            .find(|s| s.name == "scheduler.decision")
            .unwrap();
        let trace: Vec<_> = exported
            .iter()
            .filter(|s| s.span_context.trace_id() == root.span_context.trace_id())
            .collect();
        let find = |name: &str| {
            *trace
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("Missing {name}"))
        };
        assert_eq!(
            find("scheduler.evaluate").parent_span_id,
            root.span_context.span_id()
        );
        assert_eq!(
            find("scheduler.apply").parent_span_id,
            root.span_context.span_id()
        );
        assert_eq!(
            find("reflex.evaluate").parent_span_id,
            find("scheduler.evaluate").span_context.span_id()
        );
        assert_eq!(
            find("typesafe.system_one").parent_span_id,
            find("reflex.evaluate").span_context.span_id()
        );
        assert_eq!(
            find("typesafe.http_attempt").parent_span_id,
            find("typesafe.system_one").span_context.span_id()
        );
        assert_eq!(
            find("reflex.execute").parent_span_id,
            find("scheduler.apply").span_context.span_id()
        );
        assert!(root
            .attributes
            .iter()
            .any(|a| a.key.as_str() == "status" && a.value.to_string() == expected));
    }
    drop(runtime);
    tracer.shutdown().unwrap();
}
