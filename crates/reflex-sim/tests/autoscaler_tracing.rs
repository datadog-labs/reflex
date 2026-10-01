// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

// Kept in its own test binary: span callsites are cached process-wide, so a trace
// assertion must not share a process with tests that run without a subscriber.
use reflex_sim::{
    autoscaler::{judge::LiveEvaluator, Command, Scenario, Session},
    playground::inference::JevSettings,
};
use std::{sync::Arc, time::Duration};

fn settings() -> JevSettings {
    JevSettings {
        dispatch_interval: Duration::ZERO,
        ..Default::default()
    }
}
/// A loopback TypeSafe endpoint that always answers `no_change`.
async fn typesafe() -> (String, tokio::task::JoinHandle<()>) {
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/systemone",
        post(|Json(body): Json<serde_json::Value>| async move {
            let labels = body["questions"]["action"]["criteria"].as_object().unwrap();
            let probabilities: serde_json::Map<_, _> = labels
                .keys()
                .map(|k| (k.clone(), serde_json::json!(if k == "no_change" { 1.0 } else { 0.0 })))
                .collect();
            Json(serde_json::json!({"model":"jev-test","usage":{"input_tokens":5,"output_tokens":1},
                "answers":{"action":{"type":"choice","choice":"no_change","probabilities":probabilities}}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/systemone", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (endpoint, server)
}
async fn decide(s: &mut Session) {
    s.command(Command::Step).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while s.view().pending.is_some_and(|p| !p.response_ready) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.decisions()[0].status, "unchanged");
}

#[test]
fn a_decision_is_one_trace_across_evaluation_and_guarded_execution() {
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
            .with(tracing_opentelemetry::layer().with_tracer(tracer.tracer("autoscaler-test"))),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let run = runtime.block_on(
        async {
            let (endpoint, server) = typesafe().await;
            let client = typesafe_ai::TypeSafeClient::builder()
                .api_key("test-key")
                .endpoint(endpoint)
                .max_retries(0)
                .build()
                .unwrap();
            let judge = Arc::new(LiveEvaluator::new(client, "jev-test".into()));
            let mut s = Session::new(42, Some(judge), settings()).unwrap();
            s.command(Command::Scenario {
                scenario: Scenario::SurgeRecovery,
            })
            .await
            .unwrap();
            decide(&mut s).await;
            server.abort();
            s.simulation_run().to_owned()
        }
        .with_subscriber(dispatch.clone()),
    );
    tracer.force_flush().unwrap();
    let exported = spans.get_finished_spans().unwrap();
    let root = exported
        .iter()
        .find(|s| s.name == "autoscaler.decision")
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
    for (child, parent) in [
        ("autoscaler.evaluate", "autoscaler.decision"),
        ("autoscaler.apply", "autoscaler.decision"),
        ("reflex.evaluate", "autoscaler.evaluate"),
        ("typesafe.system_one", "reflex.evaluate"),
        ("typesafe.http_attempt", "typesafe.system_one"),
        ("reflex.execute", "autoscaler.apply"),
    ] {
        assert_eq!(
            find(child).parent_span_id,
            find(parent).span_context.span_id(),
            "{child} under {parent}"
        );
    }
    let attribute = |key: &str| {
        root.attributes
            .iter()
            .find(|a| a.key.as_str() == key)
            .map(|a| a.value.to_string())
    };
    assert_eq!(attribute("status").as_deref(), Some("unchanged"));
    assert_eq!(attribute("simulation_run"), Some(run));
    assert_eq!(attribute("phase").as_deref(), Some("stable"));
    drop(runtime);
    tracer.shutdown().unwrap();
}
