use axum::{routing::post, Json, Router};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use reflex_sim::{
    playground::inference::JevSettings,
    recovery::{engine::Faults, judge::LiveEvaluator, Command, Session},
};
use std::{sync::Arc, time::Duration};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;
#[test]
fn recovery_trace_survives_pause_rejection_error_and_cancellation() {
    let spans = InMemorySpanExporter::default();
    let tracer = SdkTracerProvider::builder()
        .with_simple_exporter(spans.clone())
        .build();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(tracer.tracer("recovery-test"))),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for expected in [
        "applied",
        "rejected",
        "evaluation_error",
        "cancelled",
        "cancelled_in_flight",
    ] {
        spans.reset();
        runtime.block_on(async {
            let started=Arc::new(tokio::sync::Notify::new()); let arrived=started.clone();
            let app=Router::new().route("/systemone",post(move |Json(body):Json<serde_json::Value>| { let arrived=arrived.clone(); async move {
                arrived.notify_one();
                if expected=="cancelled_in_flight" { std::future::pending::<()>().await; }
                if expected=="evaluation_error" { return (axum::http::StatusCode::SERVICE_UNAVAILABLE,Json(serde_json::json!({"error":"unavailable"}))); }
                let choices=body["questions"]["intervention"]["criteria"].as_object().unwrap();
                let selected=choices.keys().find(|key|key.contains("set_retry_budget")).unwrap();
                let probabilities:serde_json::Map<String,serde_json::Value>=choices.keys().map(|key|(key.clone(),serde_json::json!(if key==selected {1.} else {0.}))).collect();
                (axum::http::StatusCode::OK,Json(serde_json::json!({"model":"jev-test","usage":{"input_tokens":5,"output_tokens":1},"answers":{"intervention":{"type":"choice","choice":selected,"probabilities":probabilities}}})))
            }}));
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint=format!("http://{}/systemone",listener.local_addr().unwrap());
            let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
            let client=typesafe_ai::TypeSafeClient::builder().api_key("test-key").endpoint(endpoint).max_retries(0).build().unwrap();
            let mut s=Session::new(42,Some(Arc::new(LiveEvaluator::new(client,"jev-test".into()))),JevSettings { dispatch_interval:Duration::from_secs(60),..Default::default() }).unwrap();
            s.command(Command::Step).await.unwrap();
            tokio::time::timeout(Duration::from_secs(3),started.notified()).await.unwrap();
            if expected!="cancelled_in_flight" {
                tokio::time::timeout(Duration::from_secs(3),async { while !s.view().response_ready { tokio::task::yield_now().await; } }).await.unwrap();
            }
            assert!(s.view().decisions.is_empty());
            assert!(!spans.get_finished_spans().unwrap().iter().any(|s|s.name=="recovery.decision"));
            if expected=="rejected" { s.command(Command::Fault { replica:0,faults:Faults { crashed:true,..Default::default() } }).await.unwrap(); }
            if expected.starts_with("cancelled") { s.command(Command::Reset).await.unwrap(); }
            else { s.command(Command::Step).await.unwrap(); assert_eq!(s.view().decisions[0].status,expected); }
            tokio::time::timeout(Duration::from_secs(3),async { while !spans.get_finished_spans().unwrap().iter().any(|s|s.name=="recovery.decision") { tokio::task::yield_now().await; } }).await.unwrap();
            server.abort();
        }.with_subscriber(dispatch.clone()));
        tracer.force_flush().unwrap();
        let exported = spans.get_finished_spans().unwrap();
        let root = exported
            .iter()
            .find(|s| s.name == "recovery.decision")
            .unwrap();
        let find = |name: &str| {
            exported
                .iter()
                .find(|s| {
                    s.name == name && s.span_context.trace_id() == root.span_context.trace_id()
                })
                .unwrap_or_else(|| panic!("Missing {name} for {expected}"))
        };
        assert_eq!(
            find("recovery.evaluate").parent_span_id,
            root.span_context.span_id()
        );
        assert_eq!(
            find("reflex.evaluate").parent_span_id,
            find("recovery.evaluate").span_context.span_id()
        );
        assert_eq!(
            find("typesafe.system_one").parent_span_id,
            find("reflex.evaluate").span_context.span_id()
        );
        assert_eq!(
            find("typesafe.http_attempt").parent_span_id,
            find("typesafe.system_one").span_context.span_id()
        );
        if !expected.starts_with("cancelled") {
            assert_eq!(
                find("recovery.apply").parent_span_id,
                root.span_context.span_id()
            );
            assert_eq!(
                find("reflex.execute").parent_span_id,
                find("recovery.apply").span_context.span_id()
            );
        }
        let status = if expected.starts_with("cancelled") {
            "cancelled"
        } else {
            expected
        };
        assert!(root
            .attributes
            .iter()
            .any(|a| a.key.as_str() == "status" && a.value.to_string() == status));
    }
    drop(runtime);
    tracer.shutdown().unwrap();
}
