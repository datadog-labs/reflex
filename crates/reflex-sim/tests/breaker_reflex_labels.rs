// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! The breaker's controller and state machines read the global meter, so this test installs
//! a global provider and lives in its own test binary.
#[allow(dead_code)]
#[path = "support/metrics.rs"]
mod metrics;
use metrics::{count, Capture};
use reflex_sim::{
    jev::{Choice, Evaluator, JevPolicy, LiveEvaluator},
    policy::{ClientOutcome, Observation, Policy, ThresholdConfig, ThresholdPolicy},
};

fn failed(request_id: usize) -> Observation {
    Observation {
        at_ms: request_id as f64 * 20.0,
        request_id,
        outcome: ClientOutcome::Timeout,
        latency_ms: 650.0,
        generation: 0,
    }
}

#[tokio::test]
async fn every_breaker_reflex_counter_is_named_circuit_breaker() {
    use axum::{routing::post, Json, Router};
    let capture = Capture::new();
    opentelemetry::global::set_meter_provider(capture.provider.clone());

    // Both policies' state machines, including the Datadog-evidence variant of the Jev one.
    let mut threshold = ThresholdPolicy::new(ThresholdConfig::default()).unwrap();
    let mut jev = JevPolicy::new().unwrap();
    let mut remote = JevPolicy::datadog().unwrap();
    for i in 0..20 {
        threshold.observe(failed(i)).await.unwrap();
        jev.observe(failed(i)).await.unwrap();
        remote.observe(failed(i)).await.unwrap();
    }

    // The Jev policy's controller, against a loopback TypeSafe endpoint.
    let app = Router::new().route(
        "/systemone",
        post(|| async {
            Json(
                serde_json::json!({"model":"jev-test","usage":{"input_tokens":5,"output_tokens":1},
                "answers":{"action":{"type":"choice","choice":"no_change",
                    "probabilities":{"open":0.1,"probe":0.1,"no_change":0.8}}}}),
            )
        }),
    );
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
    let evaluator = LiveEvaluator::new(client, "jev-test".into());
    let result = evaluator.evaluate(jev.evidence(400.0).unwrap()).await;
    assert_eq!(result.action, Some(Choice::NoChange));
    server.abort();

    let m = capture.read();
    let transitions = count(&m, "reflex.transitions", &[("machine", "circuit_breaker")]);
    assert!(transitions >= 60, "three machines saw twenty inputs each");
    assert_eq!(
        count(&m, "reflex.transitions", &[]),
        transitions,
        "no breaker transition is left unnamed"
    );
    let named = [("controller", "circuit_breaker"), ("status", "proposed")];
    assert_eq!(count(&m, "reflex.evaluations", &named), 1);
    assert_eq!(
        count(&m, "reflex.evaluations", &[]),
        1,
        "no breaker evaluation is left unnamed"
    );
}
