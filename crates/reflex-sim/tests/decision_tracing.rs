use axum::{routing::post, Json, Router};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use reflex_sim::{
    jev::{LiveEvaluator, PolicyKind},
    playground::{inference::JevSettings, Command, Session},
};
use std::{sync::Arc, time::Duration};
use tracing::{instrument::WithSubscriber, Dispatch};
use tracing_subscriber::prelude::*;

fn attr(span: &SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|a| a.key.as_str() == key)
        .map(|a| a.value.to_string())
}
async fn ready(session: &Session) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !session
            .view()
            .inference
            .pending
            .as_ref()
            .is_some_and(|p| p.response_ready)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn new_session(
    choice: &'static str,
    fail: bool,
) -> (
    Session,
    tokio::task::JoinHandle<()>,
    Arc<tokio::sync::Notify>,
) {
    let started = Arc::new(tokio::sync::Notify::new());
    let arrived = started.clone();
    let app = Router::new().route("/systemone", post(move || { let arrived = arrived.clone(); async move {
        arrived.notify_one();
        if choice == "wait" { std::future::pending::<()>().await; }
        if fail { (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"unavailable"}))) }
        else { (axum::http::StatusCode::OK, Json(serde_json::json!({"model":"jev-test", "usage":{"input_tokens":10,"output_tokens":2},
            "answers":{"action":{"type":"choice", "choice":choice, "probabilities":{"open":0.5,"probe":0.5,"no_change":0.0}}}}))) }
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/systemone", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = typesafe_ai::TypeSafeClient::builder()
        .api_key("test-key")
        .endpoint(endpoint)
        .max_retries(0)
        .build()
        .unwrap();
    let session = Session::configured(
        42,
        PolicyKind::Jev,
        Some(Arc::new(LiveEvaluator::new(client, "jev-test".into()))),
        JevSettings {
            model: "jev-test".into(),
            // Keep this trace fixture to one evaluation without relying on a call cap.
            dispatch_interval: Duration::from_secs(3600),
        },
    )
    .unwrap();
    (session, server, started)
}

#[test]
fn decision_trace_survives_background_inference_pause_apply_failure_and_reset() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let dispatch = Dispatch::new(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("decision-test"))),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    for (choice, fail, expected) in [
        ("open", false, "applied"),
        ("probe", false, "rejected"),
        ("open", true, "evaluation_error"),
    ] {
        exporter.reset();
        runtime.block_on(
            async {
                let (mut session, server, _) = new_session(choice, fail).await;
                // This context is scoped to the caller. Spawned workers must carry it themselves.
                session.command(Command::Step).await.unwrap();
                ready(&session).await;
                assert!(session.view().decisions.is_empty());
                provider.force_flush().unwrap();
                assert!(
                    !exporter
                        .get_finished_spans()
                        .unwrap()
                        .iter()
                        .any(|s| s.name == "circuit_breaker.decision"),
                    "Parent must remain alive until application"
                );
                session.command(Command::Step).await.unwrap();
                assert_eq!(
                    session.view().decisions[0].guard.status,
                    expected,
                    "{:?}",
                    session.view().decisions[0].inference.error
                );
                server.abort();
            }
            .with_subscriber(dispatch.clone()),
        );
        provider.force_flush().unwrap();
        let spans = exporter.get_finished_spans().unwrap();
        let root = spans
            .iter()
            .find(|s| s.name == "circuit_breaker.decision")
            .expect("decision parent exported");
        assert_eq!(attr(root, "status").as_deref(), Some(expected));
        assert_eq!(attr(root, "upstream").as_deref(), Some("catalog"));
        let trace: Vec<_> = spans
            .iter()
            .filter(|s| s.span_context.trace_id() == root.span_context.trace_id())
            .collect();
        let find = |name: &str| {
            *trace
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("Missing {name} from decision trace"))
        };
        let eval = find("circuit_breaker.evaluate");
        let controller = find("reflex.evaluate");
        let client = find("typesafe.system_one");
        let http = find("typesafe.http_attempt");
        let apply = find("circuit_breaker.apply");
        assert_eq!(eval.parent_span_id, root.span_context.span_id());
        assert_eq!(apply.parent_span_id, root.span_context.span_id());
        assert_eq!(controller.parent_span_id, eval.span_context.span_id());
        assert_eq!(client.parent_span_id, controller.span_context.span_id());
        assert_eq!(http.parent_span_id, client.span_context.span_id());
        let executions: Vec<_> = trace
            .iter()
            .filter(|s| s.name == "reflex.execute")
            .collect();
        assert_eq!(
            executions.len(),
            2,
            "Clock update and guarded action share application context"
        );
        for execution in executions {
            assert_eq!(execution.parent_span_id, apply.span_context.span_id());
        }
        assert!(root.start_time <= eval.start_time && root.end_time >= apply.end_time);
    }
    // Discard a ready result outside its original dispatcher; it must close, not leak,
    // and must not gain a spurious apply span or become the next decision's parent.
    exporter.reset();
    let (mut session, server) = runtime.block_on(
        async {
            let (mut session, server, _) = new_session("open", false).await;
            session.command(Command::Step).await.unwrap();
            ready(&session).await;
            (session, server)
        }
        .with_subscriber(dispatch.clone()),
    );
    runtime.block_on(session.command(Command::Reset)).unwrap();
    server.abort();
    drop(session);
    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    let root = spans
        .iter()
        .find(|s| s.name == "circuit_breaker.decision")
        .unwrap();
    assert_eq!(attr(root, "status"), Some("cancelled".into()));
    assert!(!spans.iter().any(|s| s.name == "circuit_breaker.apply"));
    exporter.reset();
    let (mut active, server) = runtime.block_on(
        async {
            let (mut active, server, started) = new_session("wait", false).await;
            active.command(Command::Step).await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), started.notified())
                .await
                .unwrap();
            (active, server)
        }
        .with_subscriber(dispatch.clone()),
    );
    runtime.block_on(active.command(Command::Reset)).unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !exporter
                .get_finished_spans()
                .unwrap()
                .iter()
                .any(|s| s.name == "circuit_breaker.decision")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    server.abort();
    drop(active);
    let spans = exporter.get_finished_spans().unwrap();
    let root = spans
        .iter()
        .find(|s| s.name == "circuit_breaker.decision")
        .unwrap();
    assert_eq!(attr(root, "status"), Some("cancelled".into()));
    let attempt = spans
        .iter()
        .find(|s| s.name == "typesafe.http_attempt")
        .unwrap();
    assert_eq!(
        attempt.span_context.trace_id(),
        root.span_context.trace_id()
    );
    assert!(!spans.iter().any(|s| s.name == "circuit_breaker.apply"));
    drop(runtime);
    provider.shutdown().unwrap();
}
